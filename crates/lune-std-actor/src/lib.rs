#![allow(clippy::cargo_common_metadata)]

use std::{cell::RefCell, rc::Rc, sync::Arc};

use mlua::prelude::*;

use lune_utils::{TableBuilder, actor::ActorWorkerMarker};

mod bindings;
mod handles;
mod interrupt;
mod link;
mod mailbox;
mod shared_table;
mod transfer;

pub use self::link::{ActorLink, ActorOutcome, TERMINATED_MESSAGE, spawn_actor_thread};
pub use self::shared_table::SharedTable;
pub use self::transfer::Transfer;

const TYPEDEFS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/types.d.luau"));

type LaunchFn = dyn Fn(&Lua, &str, Arc<ActorLink>) -> LuaResult<()>;

/**
    Starts actors for a Lua state. The runtime that embeds this library provides it,
    since starting an actor means creating a whole new runtime on its own OS thread.
*/
#[derive(Clone)]
pub struct ActorLauncher(Rc<LaunchFn>);

impl ActorLauncher {
    /**
        Creates a launcher from a function that receives the requested script path and
        the link to the new actor. It must start the actor's thread before returning.
    */
    pub fn new(launch: impl Fn(&Lua, &str, Arc<ActorLink>) -> LuaResult<()> + 'static) -> Self {
        Self(Rc::new(launch))
    }
}

/// The link of the actor that a Lua state runs as, if any.
#[derive(Clone)]
struct WorkerContext(Arc<ActorLink>);

/// Every actor started from a Lua state, so that they can be shut down with it.
#[derive(Default)]
struct ActorChildren(RefCell<Vec<Arc<ActorLink>>>);

/**
    Returns a string containing type definitions for the `actor` standard library.
*/
#[must_use]
pub fn typedefs() -> String {
    TYPEDEFS.to_string()
}

/**
    Marks the given Lua state as the VM of an actor, before its libraries are created.
*/
pub fn install_worker(lua: &Lua, link: Arc<ActorLink>) {
    lua.set_app_data(ActorWorkerMarker);
    lua.set_app_data(WorkerContext(link));
}

/**
    Terminates every actor started from the given Lua state that is still running, and
    waits for their OS threads to exit.

    Runtimes call this once their script finishes, so that no actor outlives its owner.
*/
pub fn shutdown(lua: &Lua) {
    let children = match lua.app_data_ref::<ActorChildren>() {
        Some(children) => children.0.take(),
        None => return,
    };
    for child in &children {
        child.inbox.close();
        child.terminate();
    }
    for child in children {
        if let Some(thread) = child.take_thread() {
            // Panics inside the actor were already reported through its outcome.
            let _ = thread.join();
        }
    }
}

/**
    Returns how many actors started from the given Lua state may still have a running
    OS thread.
*/
#[must_use]
pub fn running_actor_count(lua: &Lua) -> usize {
    lua.app_data_ref::<ActorChildren>().map_or(0, |children| {
        children
            .0
            .borrow()
            .iter()
            .filter(|child| child.has_running_thread())
            .count()
    })
}

fn actor_spawn(
    lua: &Lua,
    (path, options): (String, Option<LuaTable>),
) -> LuaResult<LuaAnyUserData> {
    let launcher = lua
        .app_data_ref::<ActorLauncher>()
        .map(|launcher| launcher.clone())
        .ok_or_else(|| LuaError::runtime("Actors are unavailable in this runtime."))?;
    let mut args = Transfer::Table {
        entries: Vec::new(),
        frozen: false,
    };
    if let Some(options) = options {
        for pair in options.pairs::<String, LuaValue>() {
            let (key, value) = pair?;
            match key.as_str() {
                "Args" => {
                    if !value.is_table() {
                        return Err(LuaError::runtime("The Args option must be a table."));
                    }
                    args = Transfer::from_lua(&value)?;
                }
                other => {
                    return Err(LuaError::runtime(format!(
                        "Unknown actor option \"{other}\". The only option is \"Args\"."
                    )));
                }
            }
        }
    }
    let link = Arc::new(ActorLink::new(args));
    (launcher.0)(lua, &path, Arc::clone(&link))?;
    if lua.app_data_ref::<ActorChildren>().is_none() {
        lua.set_app_data(ActorChildren::default());
    }
    lua.app_data_ref::<ActorChildren>()
        .expect("children were registered above")
        .0
        .borrow_mut()
        .push(Arc::clone(&link));
    lua.create_userdata(handles::ActorHandle::new(link))
}

/**
    Creates the `actor` standard library module.

    # Errors

    Errors when out of memory.
*/
pub fn module(lua: Lua) -> LuaResult<LuaTable> {
    let context = lua
        .app_data_ref::<WorkerContext>()
        .map(|context| context.clone());
    let args = match &context {
        Some(WorkerContext(link)) => link.args.to_lua(&lua)?,
        None => LuaValue::Table(lua.create_table()?),
    };
    let parent = match &context {
        Some(WorkerContext(link)) => {
            LuaValue::UserData(lua.create_userdata(handles::ParentHandle::new(Arc::clone(link)))?)
        }
        None => LuaValue::Nil,
    };
    let bind_link = context.map(|WorkerContext(link)| link);
    TableBuilder::new(lua.clone())?
        .with_value("Args", args)?
        .with_function(
            "bindToMessage",
            move |lua, (topic, callback): (String, LuaFunction)| {
                let link = bind_link.as_ref().ok_or_else(|| {
                    LuaError::runtime("actor.bindToMessage is only available inside an actor.")
                })?;
                bindings::bind(lua, link, topic, callback)
            },
        )?
        .with_value("isWorker", parent.is_userdata())?
        .with_value("parent", parent)?
        .with_value("SharedTable", shared_table::library(lua.clone())?)?
        .with_function("spawn", actor_spawn)?
        .build_readonly()
}
