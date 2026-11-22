#![allow(clippy::cargo_common_metadata)]

use std::sync::OnceLock;

use mlua::prelude::*;
use mlua_luau_scheduler::LuaSpawnExt;

use lune_roblox::{
    document::{Document, DocumentError, DocumentFormat, DocumentKind},
    instance::{
        Instance, instance_to_lua, instances_to_lua, register_custom_class,
        registry::InstanceRegistry,
    },
    reflection::Database as ReflectionDatabase,
};

static REFLECTION_DATABASE: OnceLock<ReflectionDatabase> = OnceLock::new();

use lune_utils::TableBuilder;
use roblox_install::RobloxStudio;

const TYPEDEFS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/types.d.luau"));

/**
    Returns a string containing type definitions for the `roblox` standard library.
*/
#[must_use]
pub fn typedefs() -> String {
    TYPEDEFS.to_string()
}

/**
    Creates the `roblox` standard library module.

    # Errors

    Errors when out of memory.
*/
pub fn module(lua: Lua) -> LuaResult<LuaTable> {
    let mut roblox_constants = Vec::new();

    let roblox_module = lune_roblox::module(lua.clone())?;
    for pair in roblox_module.pairs::<LuaValue, LuaValue>() {
        roblox_constants.push(pair?);
    }

    TableBuilder::new(lua)?
        .with_values(roblox_constants)?
        .with_async_function("deserializePlace", deserialize_place)?
        .with_async_function("deserializeModel", deserialize_model)?
        .with_async_function("serializePlace", serialize_place)?
        .with_async_function("serializeModel", serialize_model)?
        .with_function("getAuthCookie", get_auth_cookie)?
        .with_function("getReflectionDatabase", get_reflection_database)?
        .with_function("implementProperty", implement_property)?
        .with_function("implementMethod", implement_method)?
        .with_function("registerClass", register_class)?
        .with_function("registerService", register_service)?
        .with_function("studioApplicationPath", studio_application_path)?
        .with_function("studioContentPath", studio_content_path)?
        .with_function("studioPluginPath", studio_plugin_path)?
        .with_function("studioBuiltinPluginPath", studio_builtin_plugin_path)?
        .build_readonly()
}

fn document_gc_budget(bytes: usize) -> i32 {
    i32::try_from(bytes.div_ceil(1024)).unwrap_or(i32::MAX)
}

fn collect_document_garbage(lua: &Lua, bytes: usize) -> LuaResult<()> {
    let budget = document_gc_budget(bytes);
    if budget == 0 {
        return Ok(());
    }

    // Native documents do not advance Luau's allocation-driven collector. Charge their
    // serialized size as incremental work instead of collecting the whole heap per load.
    // SAFETY: exec_raw locks and protects the state. The budget fits Lua's signed API,
    // and checking stopped state under that same lock preserves explicit GC suspension.
    unsafe {
        lua.exec_raw::<()>((), |state| {
            if mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCISRUNNING, 0) != 0 {
                mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCSTEP, budget);
            }
        })
    }
}

async fn deserialize_place(lua: Lua, contents: LuaString) -> LuaResult<LuaValue> {
    collect_document_garbage(&lua, contents.as_bytes().len())?;
    let bytes = contents.as_bytes().to_vec();
    let fut = lua.spawn_blocking(move || {
        let doc = Document::from_bytes(bytes, DocumentKind::Place)?;
        let data_model = doc.into_data_model_instance()?;
        Ok::<_, DocumentError>(data_model)
    });
    let data_model = fut.await.into_lua_err()?;
    instance_to_lua(&lua, data_model)
}

async fn deserialize_model(lua: Lua, contents: LuaString) -> LuaResult<LuaValue> {
    collect_document_garbage(&lua, contents.as_bytes().len())?;
    let bytes = contents.as_bytes().to_vec();
    let fut = lua.spawn_blocking(move || {
        let doc = Document::from_bytes(bytes, DocumentKind::Model)?;
        let instance_array = doc.into_instance_array()?;
        Ok::<_, DocumentError>(instance_array)
    });
    let instance_array = fut.await.into_lua_err()?;
    instances_to_lua(&lua, instance_array)
}

async fn serialize_place(
    lua: Lua,
    (data_model, as_xml): (LuaUserDataRef<Instance>, Option<bool>),
) -> LuaResult<LuaString> {
    let data_model = *data_model;
    let fut = lua.spawn_blocking(move || {
        let doc = Document::from_data_model_instance(data_model)?;
        let bytes = doc.to_bytes_with_format(match as_xml {
            Some(true) => DocumentFormat::Xml,
            _ => DocumentFormat::Binary,
        })?;
        Ok::<_, DocumentError>(bytes)
    });
    let bytes = fut.await.into_lua_err()?;
    lua.create_string(bytes)
}

async fn serialize_model(
    lua: Lua,
    (instances, as_xml): (Vec<LuaUserDataRef<Instance>>, Option<bool>),
) -> LuaResult<LuaString> {
    let instances = instances.iter().map(|i| **i).collect();
    let fut = lua.spawn_blocking(move || {
        let doc = Document::from_instance_array(instances)?;
        let bytes = doc.to_bytes_with_format(match as_xml {
            Some(true) => DocumentFormat::Xml,
            _ => DocumentFormat::Binary,
        })?;
        Ok::<_, DocumentError>(bytes)
    });
    let bytes = fut.await.into_lua_err()?;
    lua.create_string(bytes)
}

fn get_auth_cookie(_: &Lua, raw: Option<bool>) -> LuaResult<Option<String>> {
    if matches!(raw, Some(true)) {
        Ok(rbx_cookie::get_value())
    } else {
        Ok(rbx_cookie::get())
    }
}

fn get_reflection_database(_: &Lua, _: ()) -> LuaResult<ReflectionDatabase> {
    Ok(*REFLECTION_DATABASE.get_or_init(ReflectionDatabase::new))
}

fn implement_property(
    lua: &Lua,
    (class_name, property_name, property_getter, property_setter): (
        String,
        String,
        LuaFunction,
        Option<LuaFunction>,
    ),
) -> LuaResult<()> {
    let property_setter = if let Some(setter) = property_setter {
        setter
    } else {
        let property_name = property_name.clone();
        lua.create_function(move |_, _: LuaMultiValue| {
            Err::<(), _>(LuaError::runtime(format!(
                "Property '{property_name}' is read-only"
            )))
        })?
    };
    InstanceRegistry::insert_property_getter(lua, &class_name, &property_name, property_getter)
        .into_lua_err()?;
    InstanceRegistry::insert_property_setter(lua, &class_name, &property_name, property_setter)
        .into_lua_err()?;
    Ok(())
}

fn implement_method(
    lua: &Lua,
    (class_name, method_name, method): (String, String, LuaFunction),
) -> LuaResult<()> {
    InstanceRegistry::insert_method(lua, &class_name, &method_name, method).into_lua_err()?;
    Ok(())
}

fn register_class(
    _: &Lua,
    (class_name, super_class_name): (String, Option<String>),
) -> LuaResult<()> {
    let super_class_name = super_class_name.unwrap_or_else(|| "Instance".to_string());
    register_custom_class(&class_name, &super_class_name, false).into_lua_err()
}

fn register_service(_: &Lua, service_name: String) -> LuaResult<()> {
    register_custom_class(&service_name, "Instance", true).into_lua_err()
}

fn studio_application_path(_: &Lua, _: ()) -> LuaResult<String> {
    RobloxStudio::locate()
        .map(|rs| rs.application_path().display().to_string())
        .map_err(LuaError::external)
}

fn studio_content_path(_: &Lua, _: ()) -> LuaResult<String> {
    RobloxStudio::locate()
        .map(|rs| rs.content_path().display().to_string())
        .map_err(LuaError::external)
}

fn studio_plugin_path(_: &Lua, _: ()) -> LuaResult<String> {
    RobloxStudio::locate()
        .map(|rs| rs.plugins_path().display().to_string())
        .map_err(LuaError::external)
}

fn studio_builtin_plugin_path(_: &Lua, _: ()) -> LuaResult<String> {
    RobloxStudio::locate()
        .map(|rs| rs.built_in_plugins_path().display().to_string())
        .map_err(LuaError::external)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct NativePayload(Arc<AtomicUsize>);

    impl LuaUserData for NativePayload {}

    impl Drop for NativePayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn document_collection_budget_rounds_up_and_fits_the_ffi() {
        for (bytes, expected) in [(0, 0), (1, 1), (1023, 1), (1024, 1), (1025, 2)] {
            assert_eq!(document_gc_budget(bytes), expected);
        }
        if let Ok(largest_budget) = usize::try_from(i32::MAX as u64 * 1024) {
            assert_eq!(document_gc_budget(largest_budget), i32::MAX);
            assert_eq!(document_gc_budget(largest_budget + 1), i32::MAX);
            assert_eq!(document_gc_budget(usize::MAX), i32::MAX);
        }
        assert!(document_gc_budget(usize::MAX) > 0);
    }

    #[test]
    fn document_collection_steps_eventually_drop_native_payloads() -> LuaResult<()> {
        for mid_cycle in [false, true] {
            let lua = Lua::new();
            lua.gc_collect()?;
            lua.gc_stop();
            let dropped = Arc::new(AtomicUsize::new(0));
            let retained = lua.create_userdata(NativePayload(dropped.clone()))?;
            let released = lua.create_userdata(NativePayload(dropped.clone()))?;
            let roots = lua.create_table()?;
            for index in 1..=1024 {
                roots.set(index, lua.create_table_from([("Value", index)])?)?;
            }
            lua.gc_restart();
            if mid_cycle {
                assert!(
                    !lua.gc_step()?,
                    "The retained heap must keep the cycle active."
                );
            }
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            drop(released);

            let calls = lua.used_memory().div_ceil(1024) * 4;
            for _ in 0..calls {
                collect_document_garbage(&lua, 1024)?;
                if dropped.load(Ordering::SeqCst) > 0 {
                    break;
                }
            }
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
            assert!(retained.is::<NativePayload>());
            assert_eq!(roots.len()?, 1024);
            assert!(lua.gc_is_running());
        }
        Ok(())
    }

    #[test]
    fn document_collection_releases_unreachable_objects_and_preserves_live_roots() -> LuaResult<()>
    {
        let lua = Lua::new();
        lua.gc_collect()?;
        lua.gc_stop();
        let retained = lua.create_table_from([("Value", 42)])?;
        let weak = lua.create_table()?;
        weak.set_metatable(Some(lua.create_table_from([("__mode", "v")])?))?;
        weak.set("retained", retained.clone())?;
        weak.set("released", lua.create_table()?)?;
        assert!(matches!(
            weak.get::<LuaValue>("released")?,
            LuaValue::Table(_)
        ));

        lua.gc_restart();
        collect_document_garbage(&lua, lua.used_memory())?;

        assert!(lua.gc_is_running());
        assert!(matches!(weak.get::<LuaValue>("released")?, LuaValue::Nil));
        assert_eq!(weak.get::<LuaTable>("retained")?.get::<i64>("Value")?, 42);
        retained.set("Value", 43)?;
        assert_eq!(weak.get::<LuaTable>("retained")?.get::<i64>("Value")?, 43);
        Ok(())
    }

    #[test]
    fn document_collection_respects_a_stopped_collector() -> LuaResult<()> {
        let lua = Lua::new();
        lua.gc_stop();
        let weak = lua.create_table()?;
        weak.set_metatable(Some(lua.create_table_from([("__mode", "v")])?))?;
        weak.set("released", lua.create_table()?)?;

        for bytes in [0, 1, lua.used_memory(), usize::MAX] {
            collect_document_garbage(&lua, bytes)?;

            assert!(!lua.gc_is_running());
            assert!(matches!(
                weak.get::<LuaValue>("released")?,
                LuaValue::Table(_)
            ));
        }
        Ok(())
    }

    #[test]
    fn document_collection_preserves_returned_function_environments() -> LuaResult<()> {
        let lua = Lua::new();
        let callback = lua
            .load("local state = 0; return function() state += 1; Count += 1; return state + Count end")
            .eval::<LuaFunction>()?;
        let environment = lua.create_table_from([("Count", 0)])?;
        callback.set_environment(environment.clone())?;

        assert_eq!(callback.call::<i64>(())?, 2);
        collect_document_garbage(&lua, lua.used_memory())?;
        assert_eq!(callback.call::<i64>(())?, 4);
        assert_eq!(environment.get::<i64>("Count")?, 2);
        Ok(())
    }
}
