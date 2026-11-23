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

/**
    Every actor started from a Lua state, so that they can be shut down with it.

    Actors that were shut down are retired, and the retired ones whose thread still runs
    are kept, so that a leaked actor thread stays observable instead of disappearing from
    the list. Dropping the Lua state also stops every actor, even when its script never
    finished, for example because an embedder dropped the future that ran it.
*/
#[derive(Default)]
struct ActorChildren {
    running: RefCell<Vec<Arc<ActorLink>>>,
    retired: RefCell<Vec<Arc<ActorLink>>>,
}

impl ActorChildren {
    fn stop_all(&self) {
        let children = self.running.take();
        for child in &children {
            child.inbox.close();
            child.terminate();
        }
        for child in &children {
            if let Some(thread) = child.take_thread() {
                // Panics inside the actor were already reported through its outcome.
                let _ = thread.join();
            }
        }
        let mut retired = self.retired.borrow_mut();
        retired.retain(|child| child.is_alive());
        retired.extend(children.into_iter().filter(|child| child.is_alive()));
    }

    fn alive_count(&self) -> usize {
        let running = self.running.borrow();
        let retired = self.retired.borrow();
        running
            .iter()
            .chain(retired.iter())
            .filter(|child| child.is_alive())
            .count()
    }
}

impl Drop for ActorChildren {
    fn drop(&mut self) {
        self.stop_all();
    }
}

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
    if let Some(children) = lua.app_data_ref::<ActorChildren>() {
        children.stop_all();
    }
}

/**
    Returns how many actors ever started from the given Lua state still have an OS
    thread running their body, including actors that were already shut down.
*/
#[must_use]
pub fn running_actor_count(lua: &Lua) -> usize {
    lua.app_data_ref::<ActorChildren>()
        .map_or(0, |children| children.alive_count())
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
                "args" => {
                    if !value.is_table() {
                        return Err(LuaError::runtime("The args option must be a table."));
                    }
                    args = Transfer::from_lua(&value)?;
                }
                other => {
                    return Err(LuaError::runtime(format!(
                        "Unknown actor option \"{other}\". The only option is \"args\"."
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
        .running
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
        .with_value("args", args)?
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

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::*;

    // An actor body that only stops once its owner terminates it.
    fn wait_until_terminated(link: Arc<ActorLink>) -> ActorOutcome {
        while !link.is_terminated() {
            thread::sleep(Duration::from_millis(1));
        }
        ActorOutcome::Failed(TERMINATED_MESSAGE.to_string())
    }

    #[test]
    fn stopping_joins_every_actor_and_drop_stops_unfinished_ones() {
        let children = ActorChildren::default();
        let links: Vec<_> = (0..3)
            .map(|_| Arc::new(ActorLink::new(Transfer::Nil)))
            .collect();
        for link in &links {
            spawn_actor_thread(Arc::clone(link), wait_until_terminated).unwrap();
            children.running.borrow_mut().push(Arc::clone(link));
        }
        assert_eq!(children.alive_count(), 3);
        children.stop_all();
        assert_eq!(children.alive_count(), 0);
        assert!(links.iter().all(|link| !link.is_alive()));

        // Dropping the children stops actors that were never shut down explicitly.
        let dropped = ActorChildren::default();
        let link = Arc::new(ActorLink::new(Transfer::Nil));
        spawn_actor_thread(Arc::clone(&link), wait_until_terminated).unwrap();
        dropped.running.borrow_mut().push(Arc::clone(&link));
        drop(dropped);
        assert!(!link.is_alive(), "dropping the owner must join its actors");
    }

    #[test]
    fn retired_actors_that_still_run_stay_counted() {
        // A thread that ignores termination for a while models a leaked actor. Retiring
        // it without joining must keep it visible rather than empty the count.
        let children = ActorChildren::default();
        let link = Arc::new(ActorLink::new(Transfer::Nil));
        let (release, released) = std::sync::mpsc::channel::<()>();
        spawn_actor_thread(Arc::clone(&link), move |_| {
            let _ = released.recv();
            ActorOutcome::Completed(Vec::new())
        })
        .unwrap();
        children.retired.borrow_mut().push(Arc::clone(&link));
        assert_eq!(children.alive_count(), 1);
        release.send(()).unwrap();
        link.take_thread().unwrap().join().unwrap();
        assert_eq!(children.alive_count(), 0);
    }
}
