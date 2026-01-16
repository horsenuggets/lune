use std::path::PathBuf;

use mlua::prelude::*;

/// Type alias matching the one in the runtime crate
type ExecutablePath = Option<PathBuf>;

/// Creates the `executable` global.
///
/// Returns the path to the current executable for standalone binaries,
/// or `nil` when running as a regular Lune script.
pub fn create(lua: Lua) -> LuaResult<LuaValue> {
    // Try to get the executable path from app_data
    let executable_path = lua
        .app_data_ref::<ExecutablePath>()
        .and_then(|path| path.clone());

    match executable_path {
        Some(path) => {
            let path_str = path.to_string_lossy().to_string();
            Ok(LuaValue::String(lua.create_string(&path_str)?))
        }
        None => Ok(LuaValue::Nil),
    }
}
