use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};

use async_channel::{Receiver, Sender};
use futures_lite::future::FutureExt;
use mlua::prelude::*;
use mlua_luau_scheduler::{LuaSchedulerExt, LuaSpawnExt};

use crate::link::ActorLink;

/// The callbacks bound to one topic, served by a single message pump.
struct TopicBindings {
    callbacks: RefCell<Vec<(u64, LuaFunction)>>,
    stop_rx: Receiver<()>,
    stop_tx: Sender<()>,
}

/// Every topic with bound callbacks inside one actor.
#[derive(Default)]
struct Bindings {
    next_id: Cell<u64>,
    topics: RefCell<HashMap<String, Rc<TopicBindings>>>,
}

fn bindings(lua: &Lua) -> Rc<Bindings> {
    if let Some(bindings) = lua.app_data_ref::<Rc<Bindings>>() {
        return Rc::clone(&bindings);
    }
    let bindings = Rc::new(Bindings::default());
    lua.set_app_data(Rc::clone(&bindings));
    bindings
}

/**
    A connection returned by `actor.bindToMessage`, like `RBXScriptConnection`.
*/
struct Connection {
    connected: Cell<bool>,
    id: u64,
    topic: String,
}

impl LuaUserData for Connection {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "ActorConnection");
        fields.add_field_method_get("Connected", |_, this| Ok(this.connected.get()));
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Disconnect", |lua, this, ()| {
            if !this.connected.replace(false) {
                return Ok(());
            }
            let bindings = bindings(lua);
            let mut topics = bindings.topics.borrow_mut();
            if let Some(topic) = topics.get(&this.topic) {
                topic
                    .callbacks
                    .borrow_mut()
                    .retain(|(id, _)| *id != this.id);
                if topic.callbacks.borrow().is_empty() {
                    // Stop the pump before another binding can start a new one, so the
                    // old pump never takes a message meant for the new callbacks.
                    topic.stop_tx.close();
                    topics.remove(&this.topic);
                }
            }
            Ok(())
        });
    }
}

/**
    Runs `callback` in a new thread for every message the owner sends on `topic`.

    Every callback bound to a topic receives its own copy of each message. The actor
    keeps running while callbacks are bound, until its owner joins or terminates it.
*/
pub(crate) fn bind(
    lua: &Lua,
    link: &Arc<ActorLink>,
    topic: String,
    callback: LuaFunction,
) -> LuaResult<LuaAnyUserData> {
    let bindings = bindings(lua);
    let id = bindings.next_id.get();
    bindings.next_id.set(id + 1);
    let existing = bindings.topics.borrow().get(&topic).cloned();
    let state = if let Some(state) = existing {
        state
    } else {
        let (stop_tx, stop_rx) = async_channel::bounded(1);
        let state = Rc::new(TopicBindings {
            callbacks: RefCell::new(Vec::new()),
            stop_rx,
            stop_tx,
        });
        bindings
            .topics
            .borrow_mut()
            .insert(topic.clone(), Rc::clone(&state));
        spawn_pump(lua, link, &topic, Rc::clone(&state));
        state
    };
    state.callbacks.borrow_mut().push((id, callback));
    lua.create_userdata(Connection {
        connected: Cell::new(true),
        id,
        topic,
    })
}

fn spawn_pump(lua: &Lua, link: &Arc<ActorLink>, topic: &str, state: Rc<TopicBindings>) {
    let receiver = link.inbox.receiver(topic);
    let pump_lua = lua.clone();
    lua.spawn_local(async move {
        loop {
            // Stopping takes priority over a waiting message.
            let stop_rx = state.stop_rx.clone();
            let stopped = async move {
                let _ = stop_rx.recv().await;
                None
            };
            let received = async { receiver.recv().await.ok() };
            let Some(message) = stopped.or(received).await else {
                break;
            };
            let callbacks: Vec<LuaFunction> = state
                .callbacks
                .borrow()
                .iter()
                .map(|(_, callback)| callback.clone())
                .collect();
            for callback in callbacks {
                let Ok(value) = message.to_lua(&pump_lua) else {
                    return;
                };
                if pump_lua.push_thread_front(callback, value).is_err() {
                    return;
                }
            }
        }
    });
}
