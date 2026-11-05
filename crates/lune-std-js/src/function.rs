use mlua::prelude::*;
use rquickjs::{Function, Persistent};

use crate::context::JsContext;

/**
    A JavaScript function exported to Luau.

    The persistent function is declared before its context so it is dropped while the `QuickJS`
    runtime is still alive.
*/
pub struct JsFunction {
    function: Persistent<Function<'static>>,
    context: JsContext,
}

impl JsFunction {
    pub fn new(context: JsContext, function: Persistent<Function<'static>>) -> Self {
        Self { function, context }
    }

    pub fn persistent(&self) -> Persistent<Function<'static>> {
        self.function.clone()
    }
}

impl LuaUserData for JsFunction {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field_with(LuaMetaMethod::Call, |lua| {
            lua.create_async_function(
                |lua, (this, arguments): (LuaAnyUserData, LuaMultiValue)| async move {
                    let (context, function) = {
                        let this = this.borrow::<JsFunction>()?;
                        (this.context.clone(), this.function.clone())
                    };
                    context.call_function(lua, function, arguments).await
                },
            )
        });
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, (): ()| {
            Ok("JavaScript function")
        });
    }
}
