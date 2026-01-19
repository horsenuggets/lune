use mlua::prelude::*;

/// Creates the `task` global, which provides the same interface as Roblox's task library.
/// This allows Roblox code to use `task.spawn`, `task.defer`, `task.wait`, etc. without
/// needing to explicitly require the module.
pub fn create(lua: Lua) -> LuaResult<LuaValue> {
    let table = lune_std_task::module(lua)?;
    Ok(LuaValue::Table(table))
}
