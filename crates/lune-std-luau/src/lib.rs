#![allow(clippy::cargo_common_metadata)]

use mlua::prelude::*;

use lune_utils::{TableBuilder, process::ProcessJitEnablement};

mod options;

use self::options::{LuauCompileOptions, LuauLoadOptions};

const TYPEDEFS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/types.d.luau"));

/**
    Returns a string containing type definitions for the `luau` standard library.
*/
#[must_use]
pub fn typedefs() -> String {
    TYPEDEFS.to_string()
}

/**
    Creates the `luau` standard library module.

    # Errors

    Errors when out of memory.
*/
pub fn module(lua: Lua) -> LuaResult<LuaTable> {
    TableBuilder::new(lua)?
        .with_function("compile", compile_source)?
        .with_function("load", load_source)?
        .build_readonly()
}

fn compile_source(
    lua: &Lua,
    (source, options): (LuaString, LuauCompileOptions),
) -> LuaResult<LuaString> {
    options
        .into_compiler()
        .compile(source.as_bytes())
        .and_then(|s| lua.create_string(s))
}

fn load_source(
    lua: &Lua,
    (source, options): (LuaString, LuauLoadOptions),
) -> LuaResult<LuaFunction> {
    let mut chunk = lua
        .load(source.as_bytes().to_vec())
        .set_name(options.debug_name);
    let env_changed = options.environment.is_some();

    if let Some(custom_environment) = options.environment {
        let environment = lua.create_table()?;

        // Inject all globals into the environment
        if options.inject_globals {
            for pair in lua.globals().pairs() {
                let (key, value): (LuaValue, LuaValue) = pair?;
                environment.set(key, value)?;
            }

            if let Some(global_metatable) = lua.globals().metatable() {
                environment.set_metatable(Some(global_metatable))?;
            }
        } else if let Some(custom_metatable) = custom_environment.metatable() {
            // Since we don't need to set the global metatable,
            // we can just set a custom metatable if it exists
            environment.set_metatable(Some(custom_metatable))?;
        }

        // Inject the custom environment
        for pair in custom_environment.pairs() {
            let (key, value): (LuaValue, LuaValue) = pair?;
            environment.set(key, value)?;
        }

        chunk = chunk.set_environment(environment);
    }

    let process_jit = lua
        .app_data_ref::<ProcessJitEnablement>()
        .ok_or(LuaError::runtime(
            "Failed to get current JitStatus ref from AppData",
        ))?
        .enabled();
    // Enable JIT if codegen is enabled and the environment hasn't
    // changed, otherwise disable JIT since it'll fall back anyways.
    lua.enable_jit(options.codegen_enabled && !env_changed);
    let function = chunk.into_function();
    lua.enable_jit(process_jit);

    function
}

#[cfg(test)]
mod tests {
    use std::os::raw::c_int;

    use super::*;

    // Luau marks loop-free top-level chunks cold and excludes them from native generation.
    const PROBE_SOURCE: &str = "local generated, native for _ = 1, 1 do generated, native = nativeState() end return generated, native";

    unsafe extern "C-unwind" fn native_state(state: *mut mlua::ffi::lua_State) -> c_int {
        // This callback is called directly by the compiled Luau chunk at stack level one.
        unsafe {
            let generated = mlua::ffi::lua_hascustomexecution(state, 1);
            let native = mlua::ffi::lua_incustomexecution(state, 1);
            mlua::ffi::lua_pushboolean(state, generated);
            mlua::ffi::lua_pushboolean(state, native);
        }
        2
    }

    fn probe_lua(process_jit: bool) -> Lua {
        let lua = Lua::new();
        lua.set_app_data(ProcessJitEnablement::new(process_jit));
        lua.enable_jit(process_jit);
        // The callback uses only its supplied state and pushes exactly two booleans.
        let probe = unsafe { lua.create_c_function(native_state) }.unwrap();
        lua.globals().set("nativeState", probe).unwrap();
        lua.globals().set_safeenv(true);
        lua
    }

    fn expected_native(enabled: bool) -> (bool, bool) {
        // This is a capability query, not an inspection of VM internals.
        let supported = unsafe { mlua::ffi::luau_codegen_supported() != 0 };
        (enabled && supported, enabled && supported)
    }

    #[test]
    fn loaded_chunks_use_requested_codegen_and_restore_process_jit() {
        for process_jit in [false, true] {
            for codegen in [false, true] {
                for custom_environment in [false, true] {
                    let lua = probe_lua(process_jit);
                    let options = lua.create_table().unwrap();
                    options.set("codegenEnabled", codegen).unwrap();
                    if custom_environment {
                        options
                            .set("environment", lua.create_table().unwrap())
                            .unwrap();
                    }
                    let load: LuaFunction = module(lua.clone()).unwrap().get("load").unwrap();
                    let loaded: LuaFunction = load.call((PROBE_SOURCE, options)).unwrap();
                    assert_eq!(
                        loaded.call::<(bool, bool)>(()).unwrap(),
                        expected_native(codegen && !custom_environment),
                    );
                    let ordinary = lua.load(PROBE_SOURCE).into_function().unwrap();
                    assert_eq!(
                        ordinary.call::<(bool, bool)>(()).unwrap(),
                        expected_native(process_jit)
                    );
                }
            }
        }
    }

    #[test]
    fn failed_loads_restore_process_jit() {
        for process_jit in [false, true] {
            for codegen in [false, true] {
                for custom_environment in [false, true] {
                    let lua = probe_lua(process_jit);
                    let result = load_source(
                        &lua,
                        (
                            lua.create_string("local = invalid").unwrap(),
                            LuauLoadOptions {
                                codegen_enabled: codegen,
                                environment: custom_environment
                                    .then(|| lua.create_table().unwrap()),
                                ..LuauLoadOptions::default()
                            },
                        ),
                    );
                    assert!(result.is_err());
                    let ordinary = lua.load(PROBE_SOURCE).into_function().unwrap();
                    assert_eq!(
                        ordinary.call::<(bool, bool)>(()).unwrap(),
                        expected_native(process_jit)
                    );
                }
            }
        }
    }
}
