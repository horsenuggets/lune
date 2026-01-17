use mlua::prelude::*;

use lune_roblox::datatypes::types::{CFrame, Color3, NumberRange, Vector2, Vector3};
use lune_roblox::exports::LuaExportsTable;

pub fn create_cframe(lua: Lua) -> LuaResult<LuaValue> {
    CFrame::create_exports_table(lua.clone())?.into_lua(&lua)
}

pub fn create_color3(lua: Lua) -> LuaResult<LuaValue> {
    Color3::create_exports_table(lua.clone())?.into_lua(&lua)
}

pub fn create_number_range(lua: Lua) -> LuaResult<LuaValue> {
    NumberRange::create_exports_table(lua.clone())?.into_lua(&lua)
}

pub fn create_vector2(lua: Lua) -> LuaResult<LuaValue> {
    Vector2::create_exports_table(lua.clone())?.into_lua(&lua)
}

pub fn create_vector3(lua: Lua) -> LuaResult<LuaValue> {
    Vector3::create_exports_table(lua.clone())?.into_lua(&lua)
}
