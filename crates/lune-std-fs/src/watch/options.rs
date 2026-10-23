use std::time::Duration;

use mlua::prelude::*;

/**
    Options for `fs.watch`.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsWatchOptions {
    pub(crate) debounce: Duration,
    pub(crate) recursive: bool,
}

impl Default for FsWatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::ZERO,
            recursive: true,
        }
    }
}

impl FsWatchOptions {
    /**
        Parses a debounce duration given in seconds.

        # Errors

        Errors if the value is negative, NaN, infinite, or too large to represent.
    */
    pub(crate) fn parse_debounce(seconds: f64) -> Result<Duration, String> {
        Duration::try_from_secs_f64(seconds).map_err(|_| {
            format!(
                "Invalid watch options - debounce must be a non-negative number of seconds, got {seconds}"
            )
        })
    }
}

impl FromLua for FsWatchOptions {
    fn from_lua(value: LuaValue, _: &Lua) -> LuaResult<Self> {
        match value {
            LuaValue::Nil => Ok(Self::default()),
            LuaValue::Table(t) => {
                let defaults = Self::default();
                let recursive: Option<bool> = t.get("recursive")?;
                let debounce: Option<f64> = t.get("debounce")?;
                let debounce = match debounce {
                    None => defaults.debounce,
                    Some(seconds) => Self::parse_debounce(seconds).map_err(LuaError::runtime)?,
                };
                Ok(Self {
                    debounce,
                    recursive: recursive.unwrap_or(defaults.recursive),
                })
            }
            _ => Err(LuaError::FromLuaConversionError {
                from: value.type_name(),
                to: "FsWatchOptions".to_string(),
                message: Some(format!(
                    "Invalid watch options - expected table, got {}",
                    value.type_name()
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_recursive_without_debounce() {
        let options = FsWatchOptions::default();
        assert!(options.recursive);
        assert_eq!(options.debounce, Duration::ZERO);
    }

    #[test]
    fn debounce_accepts_non_negative_finite_seconds() {
        assert_eq!(FsWatchOptions::parse_debounce(0.0), Ok(Duration::ZERO));
        assert_eq!(
            FsWatchOptions::parse_debounce(0.25),
            Ok(Duration::from_millis(250))
        );
    }

    #[test]
    fn debounce_rejects_invalid_seconds() {
        for seconds in [
            -0.1,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e20,
            f64::MAX,
        ] {
            assert!(FsWatchOptions::parse_debounce(seconds).is_err());
        }
    }

    #[test]
    fn from_lua_parses_every_shape() {
        let lua = Lua::new();
        let nil = FsWatchOptions::from_lua(LuaValue::Nil, &lua).unwrap();
        assert_eq!(nil, FsWatchOptions::default());

        let table: LuaTable = lua
            .load("return { recursive = false, debounce = 0.5 }")
            .eval()
            .unwrap();
        let parsed = FsWatchOptions::from_lua(LuaValue::Table(table), &lua).unwrap();
        assert!(!parsed.recursive);
        assert_eq!(parsed.debounce, Duration::from_millis(500));

        let empty = FsWatchOptions::from_lua(LuaValue::Table(lua.create_table().unwrap()), &lua);
        assert_eq!(empty.unwrap(), FsWatchOptions::default());

        let negative: LuaTable = lua.load("return { debounce = -1 }").eval().unwrap();
        assert!(FsWatchOptions::from_lua(LuaValue::Table(negative), &lua).is_err());

        assert!(FsWatchOptions::from_lua(LuaValue::Boolean(true), &lua).is_err());
    }
}
