#![allow(clippy::cargo_common_metadata)]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

use mlua::prelude::*;
use rquickjs::{AsyncContext, AsyncRuntime};

use lune_utils::TableBuilder;

mod context;
mod convert;
mod error;
mod function;
pub mod loader;
pub mod module;
mod options;
pub mod paths;
pub mod resolver;
pub mod source;
pub mod source_map;
pub mod store;
pub mod transform;

use self::{context::JsContext, convert::sentinels, options::JsContextOptions};

const TYPEDEFS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/types.d.luau"));

/**
    Returns a string containing type definitions for the `js` standard library.
*/
#[must_use]
pub fn typedefs() -> String {
    TYPEDEFS.to_string()
}

/**
    Creates the `js` standard library module.

    # Errors

    Errors when out of memory, or if a `QuickJS` runtime or context cannot be created.
*/
pub fn module(lua: Lua) -> LuaResult<LuaTable> {
    let sentinels = sentinels(&lua)?;
    TableBuilder::new(lua)?
        .with_value("Null", sentinels.null())?
        .with_value("Undefined", sentinels.undefined())?
        .with_async_function("newContext", |lua, options: JsContextOptions| async move {
            let runtime = AsyncRuntime::new().into_lua_err()?;
            options.apply(&runtime).await;
            let context = AsyncContext::full(&runtime).await.into_lua_err()?;
            let context = JsContext::new(lua.clone(), runtime, context, options)
                .await
                .map_err(LuaError::from)?;
            lua.create_userdata(context)
        })?
        .build_readonly()
}
