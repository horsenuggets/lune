use mlua::prelude::*;

const MODULE_CACHE_KEY: &str = "__LUNE_ROBLOX_MODULE";

pub fn module(lua: Lua) -> LuaResult<LuaTable> {
    if let Some(module) = lua.named_registry_value::<Option<LuaTable>>(MODULE_CACHE_KEY)? {
        return Ok(module);
    }

    let module = lune_std_roblox::module(lua.clone())?;
    lua.set_named_registry_value(MODULE_CACHE_KEY, module.clone())?;
    Ok(module)
}

pub fn inject(lua: Lua) -> LuaResult<()> {
    let globals = lua.globals();
    for pair in module(lua)?.pairs::<LuaString, LuaValue>() {
        let (key, value) = pair?;
        let existing: LuaValue = globals.raw_get(key.clone())?;
        if !matches!(existing, LuaValue::Nil) && existing != value {
            return Err(LuaError::runtime(format!(
                "Roblox export '{}' conflicts with an existing global.",
                key.to_str()?
            )));
        }
        globals.set(key, value)?;
    }
    Ok(())
}

pub fn create_cframe(lua: Lua) -> LuaResult<LuaValue> {
    module(lua)?.get("CFrame")
}

pub fn create_color3(lua: Lua) -> LuaResult<LuaValue> {
    module(lua)?.get("Color3")
}

pub fn create_number_range(lua: Lua) -> LuaResult<LuaValue> {
    module(lua)?.get("NumberRange")
}

pub fn create_vector2(lua: Lua) -> LuaResult<LuaValue> {
    module(lua)?.get("Vector2")
}

pub fn create_vector3(lua: Lua) -> LuaResult<LuaValue> {
    module(lua)?.get("Vector3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globals_share_every_module_export() -> LuaResult<()> {
        let lua = Lua::new();
        let module = module(lua.clone())?;
        inject(lua.clone())?;
        for pair in module.pairs::<LuaString, LuaValue>() {
            let (key, value) = pair?;
            assert_eq!(lua.globals().get::<LuaValue>(key)?, value);
        }
        Ok(())
    }

    #[test]
    fn repeated_injection_preserves_export_identity() -> LuaResult<()> {
        let lua = Lua::new();
        let first = module(lua.clone())?;
        inject(lua.clone())?;
        inject(lua.clone())?;
        let second = module(lua.clone())?;
        assert_eq!(first, second);
        assert_eq!(
            create_cframe(lua.clone())?,
            first.get::<LuaValue>("CFrame")?
        );
        assert_eq!(
            create_color3(lua.clone())?,
            first.get::<LuaValue>("Color3")?
        );
        assert_eq!(
            create_number_range(lua.clone())?,
            first.get::<LuaValue>("NumberRange")?
        );
        assert_eq!(
            create_vector2(lua.clone())?,
            first.get::<LuaValue>("Vector2")?
        );
        assert_eq!(
            create_vector3(lua.clone())?,
            first.get::<LuaValue>("Vector3")?
        );
        Ok(())
    }

    #[test]
    fn public_injection_protects_existing_roblox_names() -> LuaResult<()> {
        for name in [
            "CFrame",
            "Color3",
            "NumberRange",
            "Vector2",
            "Vector3",
            "Enum",
        ] {
            let lua = Lua::new();
            let _scheduler = mlua_luau_scheduler::Scheduler::new(lua.clone());
            lua.globals().set(name, "Existing global")?;
            let error = crate::inject_globals(lua.clone())
                .expect_err("A conflicting Roblox global must be rejected.");
            assert!(error.to_string().contains(&format!("'{name}' conflicts")));
            assert_eq!(lua.globals().get::<String>(name)?, "Existing global");
        }
        Ok(())
    }

    #[test]
    fn injection_does_not_replace_conflicting_globals() -> LuaResult<()> {
        let lua = Lua::new();
        lua.globals().set("Enum", "Existing enum")?;
        let error = inject(lua.clone()).expect_err("A conflicting global must be rejected.");
        assert!(error.to_string().contains("'Enum' conflicts"));
        assert_eq!(lua.globals().get::<String>("Enum")?, "Existing enum");
        Ok(())
    }
}
