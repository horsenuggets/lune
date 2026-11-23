use std::sync::Arc;

use mlua::prelude::*;
use mlua_luau_scheduler::LuaSpawnExt;

use crate::{
    link::{ActorLink, ActorOutcome},
    transfer::Transfer,
};

/**
    The owner's handle to an actor, returned by `actor.spawn`.
*/
pub(crate) struct ActorHandle {
    link: Arc<ActorLink>,
}

impl ActorHandle {
    pub(crate) fn new(link: Arc<ActorLink>) -> Self {
        Self { link }
    }
}

impl LuaUserData for ActorHandle {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Actor");
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("send", |_, this, (topic, value): (String, LuaValue)| {
            let value = Transfer::from_lua(&value)?;
            if this.link.inbox.send(&topic, value) {
                Ok(())
            } else {
                Err(LuaError::runtime(
                    "This actor no longer accepts messages, since it finished, was joined or was terminated.",
                ))
            }
        });
        methods.add_async_method("receive", |lua, this, topic: String| {
            let receiver = this.link.outbox.receiver(&topic);
            async move {
                match receiver.recv().await {
                    Ok(value) => value.to_lua(&lua),
                    Err(_) => Err(LuaError::runtime(format!(
                        "The actor finished without sending another \"{topic}\" message."
                    ))),
                }
            }
        });
        methods.add_async_method("join", |lua, this, ()| {
            let link = Arc::clone(&this.link);
            async move {
                // Joining means the owner has nothing more to send, which lets actors
                // that are waiting on bound messages finish.
                link.inbox.close();
                let outcome = link.wait().await;
                if let Some(thread) = link.take_thread() {
                    // The outcome is delivered at the very end of the thread, so joining
                    // it only waits for the thread to exit.
                    lua.spawn_blocking(move || {
                        let _ = thread.join();
                    })
                    .await;
                }
                crate::reap_children(&lua);
                let mut results = LuaMultiValue::new();
                match outcome {
                    ActorOutcome::Completed(values) => {
                        results.push_back(LuaValue::Boolean(true));
                        results.extend(Transfer::to_lua_multi(&values, &lua)?);
                    }
                    ActorOutcome::Failed(message) => {
                        results.push_back(LuaValue::Boolean(false));
                        results.push_back(LuaValue::String(lua.create_string(message)?));
                    }
                }
                Ok(results)
            }
        });
        methods.add_method("terminate", |_, this, ()| {
            this.link.terminate();
            Ok(())
        });
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| Ok("Actor"));
    }
}

/**
    An actor's handle to its owner, available as `actor.parent` inside the actor.
*/
pub(crate) struct ParentHandle {
    link: Arc<ActorLink>,
}

impl ParentHandle {
    pub(crate) fn new(link: Arc<ActorLink>) -> Self {
        Self { link }
    }
}

impl LuaUserData for ParentHandle {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "ActorParent");
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("send", |_, this, (topic, value): (String, LuaValue)| {
            let value = Transfer::from_lua(&value)?;
            if this.link.outbox.send(&topic, value) {
                Ok(())
            } else {
                Err(LuaError::runtime(
                    "The owner of this actor no longer receives messages.",
                ))
            }
        });
        methods.add_async_method("receive", |lua, this, topic: String| {
            let receiver = this.link.inbox.receiver(&topic);
            async move {
                match receiver.recv().await {
                    Ok(value) => value.to_lua(&lua),
                    Err(_) => Err(LuaError::runtime(format!(
                        "The owner stopped sending messages before another \"{topic}\" message arrived."
                    ))),
                }
            }
        });
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| Ok("ActorParent"));
    }
}
