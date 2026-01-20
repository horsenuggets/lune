use std::io::Write;

use console::style;
use lune_utils::fmt::{ValueFormatConfig, pretty_format_multi_value};
use mlua::prelude::*;

const FORMAT_CONFIG: ValueFormatConfig = ValueFormatConfig::new()
    .with_max_depth(4)
    .with_colors_enabled(false); // Disable colors since we'll wrap everything in yellow

pub fn create(lua: Lua) -> LuaResult<LuaValue> {
    let f = lua.create_function(|_: &Lua, args: LuaMultiValue| {
        let message = pretty_format_multi_value(&args, &FORMAT_CONFIG);
        // Print [WARN] prefix and message on same line, all in yellow
        let formatted = format!("{}\n", style(format!("[WARN] {}", message)).yellow());
        let mut stdout = std::io::stdout();
        stdout.write_all(formatted.as_bytes())?;
        stdout.flush()?;
        Ok(())
    })?;
    f.into_lua(&lua)
}
