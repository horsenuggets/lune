use std::str::FromStr;

use mlua::prelude::*;

/**
    A standard global provided by Lune.
*/
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum LuneStandardGlobal {
    #[cfg(feature = "roblox")]
    CFrame,
    #[cfg(feature = "roblox")]
    Color3,
    Executable,
    GTable,
    #[cfg(feature = "roblox")]
    NumberRange,
    Print,
    Require,
    Script,
    #[cfg(feature = "task")]
    Task,
    #[cfg(feature = "roblox")]
    Vector2,
    #[cfg(feature = "roblox")]
    Vector3,
    Version,
    Warn,
}

impl LuneStandardGlobal {
    /**
        All available standard globals.

        Note: `Executable` is not included here because it needs to be injected
        after app_data is set (it reads the executable path from app_data).
    */
    pub const ALL: &'static [Self] = &[
        #[cfg(feature = "roblox")]
        Self::CFrame,
        #[cfg(feature = "roblox")]
        Self::Color3,
        Self::GTable,
        #[cfg(feature = "roblox")]
        Self::NumberRange,
        Self::Print,
        Self::Require,
        Self::Script,
        #[cfg(feature = "task")]
        Self::Task,
        #[cfg(feature = "roblox")]
        Self::Vector2,
        #[cfg(feature = "roblox")]
        Self::Vector3,
        Self::Version,
        Self::Warn,
    ];

    /**
        Gets the name of the global, such as `_G` or `require`.
    */
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(feature = "roblox")]
            Self::CFrame => "CFrame",
            #[cfg(feature = "roblox")]
            Self::Color3 => "Color3",
            Self::Executable => "executable",
            Self::GTable => "_G",
            #[cfg(feature = "roblox")]
            Self::NumberRange => "NumberRange",
            Self::Print => "print",
            Self::Require => "require",
            Self::Script => "script",
            #[cfg(feature = "task")]
            Self::Task => "task",
            #[cfg(feature = "roblox")]
            Self::Vector2 => "Vector2",
            #[cfg(feature = "roblox")]
            Self::Vector3 => "Vector3",
            Self::Version => "_VERSION",
            Self::Warn => "warn",
        }
    }

    /**
        Creates the Lua value for the global.

        # Errors

        If the global could not be created.
    */
    #[rustfmt::skip]
    #[allow(unreachable_patterns)]
    pub fn create(&self, lua: Lua) -> LuaResult<LuaValue> {
        let res = match self {
            #[cfg(feature = "roblox")]
            Self::CFrame => crate::globals::roblox_globals::create_cframe(lua),
            #[cfg(feature = "roblox")]
            Self::Color3 => crate::globals::roblox_globals::create_color3(lua),
            Self::Executable => crate::globals::executable::create(lua),
            Self::GTable => crate::globals::g_table::create(lua),
            #[cfg(feature = "roblox")]
            Self::NumberRange => crate::globals::roblox_globals::create_number_range(lua),
            Self::Print => crate::globals::print::create(lua),
            Self::Require => crate::globals::require::create(lua),
            Self::Script => crate::globals::script::create(lua),
            #[cfg(feature = "task")]
            Self::Task => crate::globals::task::create(lua),
            #[cfg(feature = "roblox")]
            Self::Vector2 => crate::globals::roblox_globals::create_vector2(lua),
            #[cfg(feature = "roblox")]
            Self::Vector3 => crate::globals::roblox_globals::create_vector3(lua),
            Self::Version => crate::globals::version::create(lua),
            Self::Warn => crate::globals::warn::create(lua),
        };
        match res {
            Ok(v) => Ok(v),
            Err(e) => Err(e.context(format!(
                "Failed to create standard global '{}'",
                self.name()
            ))),
        }
    }
}

impl FromStr for LuneStandardGlobal {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let low = s.trim().to_ascii_lowercase();
        Ok(match low.as_str() {
            #[cfg(feature = "roblox")]
            "cframe" => Self::CFrame,
            #[cfg(feature = "roblox")]
            "color3" => Self::Color3,
            "executable" => Self::Executable,
            "_g" => Self::GTable,
            #[cfg(feature = "roblox")]
            "numberrange" => Self::NumberRange,
            "print" => Self::Print,
            "require" => Self::Require,
            "script" => Self::Script,
            #[cfg(feature = "task")]
            "task" => Self::Task,
            #[cfg(feature = "roblox")]
            "vector2" => Self::Vector2,
            #[cfg(feature = "roblox")]
            "vector3" => Self::Vector3,
            "_version" => Self::Version,
            "warn" => Self::Warn,
            _ => {
                return Err(format!(
                    "Unknown standard global '{low}'\nValid globals are: {}",
                    Self::ALL
                        .iter()
                        .map(Self::name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        })
    }
}
