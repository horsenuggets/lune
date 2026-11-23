use mlua::prelude::*;

/**
    Marks a Lua state that runs inside an actor instead of the main runtime.

    Libraries that must not run concurrently with the main runtime, such as interactive
    terminal prompts, check this marker to reject their use inside actors.
*/
#[derive(Debug, Clone, Copy, Default)]
pub struct ActorWorkerMarker;

/**
    Returns whether the given Lua state runs inside an actor.
*/
#[must_use]
pub fn is_actor_worker(lua: &Lua) -> bool {
    lua.app_data_ref::<ActorWorkerMarker>().is_some()
}
