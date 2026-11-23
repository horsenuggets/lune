use std::{collections::HashSet, ffi::c_void};

use mlua::prelude::*;

use crate::shared_table::SharedTable;

/// Tables nested deeper than this are rejected instead of risking a native stack overflow.
pub(crate) const MAX_DEPTH: usize = 256;

/**
    A value copied out of one Luau VM so that it can be recreated in another.

    Every variant owns its data and is safe to move between OS threads. Shared tables are
    the only variant that refers to state shared between VMs, which they guard themselves.
*/
#[derive(Debug, Clone)]
pub enum Transfer {
    Nil,
    Boolean(bool),
    Number(f64),
    Vector(LuaVector),
    String(Vec<u8>),
    Buffer(Vec<u8>),
    Table {
        entries: Vec<(Transfer, Transfer)>,
        frozen: bool,
    },
    Shared(SharedTable),
}

impl Transfer {
    /**
        Copies a Luau value so that it can be sent to another actor.

        # Errors

        Errors for values that cannot leave their VM, such as functions, threads and
        userdata like Instances, and for cyclic, overly deep or metatable-backed tables.
    */
    pub fn from_lua(value: &LuaValue) -> LuaResult<Self> {
        Self::copy(value, &mut HashSet::new(), 0)
    }

    /**
        Copies every value in a multi-value so that they can be sent to another actor.

        # Errors

        Errors under the same conditions as [`Transfer::from_lua`].
    */
    pub fn from_lua_multi(values: &LuaMultiValue) -> LuaResult<Vec<Self>> {
        values.iter().map(Self::from_lua).collect()
    }

    fn copy(
        value: &LuaValue,
        visiting: &mut HashSet<*const c_void>,
        depth: usize,
    ) -> LuaResult<Self> {
        Ok(match value {
            LuaValue::Nil => Self::Nil,
            LuaValue::Boolean(b) => Self::Boolean(*b),
            LuaValue::Integer(i) => Self::Number(*i as f64),
            LuaValue::Number(n) => Self::Number(*n),
            LuaValue::Vector(v) => Self::Vector(*v),
            LuaValue::String(s) => Self::String(s.as_bytes().to_vec()),
            LuaValue::Buffer(b) => Self::Buffer(b.to_vec()),
            LuaValue::Table(t) => Self::copy_table(t, visiting, depth)?,
            LuaValue::UserData(ud) => match ud.borrow::<SharedTable>() {
                Ok(shared) => Self::Shared(shared.clone()),
                Err(_) => {
                    return Err(LuaError::runtime(
                        "Userdata values, including Instances, cannot be sent to another actor.",
                    ));
                }
            },
            LuaValue::Function(_) => {
                return Err(LuaError::runtime(
                    "Functions cannot be sent to another actor.",
                ));
            }
            LuaValue::Thread(_) => {
                return Err(LuaError::runtime(
                    "Threads cannot be sent to another actor.",
                ));
            }
            other => {
                return Err(LuaError::runtime(format!(
                    "Values of type \"{}\" cannot be sent to another actor.",
                    other.type_name()
                )));
            }
        })
    }

    fn copy_table(
        table: &LuaTable,
        visiting: &mut HashSet<*const c_void>,
        depth: usize,
    ) -> LuaResult<Self> {
        if depth >= MAX_DEPTH {
            return Err(LuaError::runtime(format!(
                "Tables nested more than {MAX_DEPTH} levels deep cannot be sent to another actor."
            )));
        }
        if table.metatable().is_some() {
            return Err(LuaError::runtime(
                "Tables with metatables cannot be sent to another actor.",
            ));
        }
        let pointer = table.to_pointer();
        if !visiting.insert(pointer) {
            return Err(LuaError::runtime(
                "Cyclic tables cannot be sent to another actor.",
            ));
        }
        let mut entries = Vec::new();
        for pair in table.pairs::<LuaValue, LuaValue>() {
            let (key, value) = pair?;
            let key = match &key {
                LuaValue::Boolean(_)
                | LuaValue::Integer(_)
                | LuaValue::Number(_)
                | LuaValue::String(_) => Self::copy(&key, visiting, depth + 1)?,
                _ => {
                    return Err(LuaError::runtime(
                        "Only strings, numbers and booleans can be table keys in actor messages.",
                    ));
                }
            };
            entries.push((key, Self::copy(&value, visiting, depth + 1)?));
        }
        // Shared references to the same acyclic table are copied separately.
        visiting.remove(&pointer);
        Ok(Self::Table {
            entries,
            frozen: table.is_readonly(),
        })
    }

    /**
        Recreates the copied value inside the given Luau VM.

        # Errors

        Errors when out of memory.
    */
    pub fn to_lua(&self, lua: &Lua) -> LuaResult<LuaValue> {
        Ok(match self {
            Self::Nil => LuaValue::Nil,
            Self::Boolean(b) => LuaValue::Boolean(*b),
            Self::Number(n) => LuaValue::Number(*n),
            Self::Vector(v) => LuaValue::Vector(*v),
            Self::String(s) => LuaValue::String(lua.create_string(s)?),
            Self::Buffer(b) => LuaValue::Buffer(lua.create_buffer(b)?),
            Self::Table { entries, frozen } => {
                let table = lua.create_table_with_capacity(0, entries.len())?;
                for (key, value) in entries {
                    table.raw_set(key.to_lua(lua)?, value.to_lua(lua)?)?;
                }
                if *frozen {
                    table.set_readonly(true);
                }
                LuaValue::Table(table)
            }
            Self::Shared(shared) => LuaValue::UserData(lua.create_userdata(shared.clone())?),
        })
    }

    /**
        Recreates every copied value inside the given Luau VM.

        # Errors

        Errors when out of memory.
    */
    pub fn to_lua_multi(values: &[Self], lua: &Lua) -> LuaResult<LuaMultiValue> {
        values.iter().map(|value| value.to_lua(lua)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(lua: &Lua, source: &str) -> LuaValue {
        lua.load(source).eval().expect("source evaluates")
    }

    fn rejection(lua: &Lua, source: &str) -> String {
        Transfer::from_lua(&eval(lua, source))
            .expect_err("value must be rejected")
            .to_string()
    }

    #[test]
    fn copies_values_between_vms() {
        let source = Lua::new();
        let target = Lua::new();
        let value = eval(
            &source,
            r#"
                local nested = table.freeze({ "frozen" })
                return { 1, nil, 3, name = "Value", [true] = vector.create(1, 2, 3),
                    data = buffer.fromstring("bytes"), nested = nested }
            "#,
        );
        let copied = Transfer::from_lua(&value).expect("value copies");
        target
            .globals()
            .set("copied", copied.to_lua(&target).unwrap())
            .unwrap();
        let checks: bool = target
            .load(
                r#"
                    return copied[1] == 1 and copied[2] == nil and copied[3] == 3
                        and copied.name == "Value" and copied[true] == vector.create(1, 2, 3)
                        and buffer.tostring(copied.data) == "bytes"
                        and table.isfrozen(copied.nested) and copied.nested[1] == "frozen"
                        and not table.isfrozen(copied)
                "#,
            )
            .eval()
            .unwrap();
        assert!(checks);
    }

    #[test]
    fn copies_are_independent_of_the_source() {
        let lua = Lua::new();
        let value = eval(&lua, "source = { Count = 1 } return source");
        let copied = Transfer::from_lua(&value).unwrap().to_lua(&lua).unwrap();
        lua.globals().set("copied", copied).unwrap();
        let unchanged: bool = lua
            .load("source.Count = 2 return copied.Count == 1 and copied ~= source")
            .eval()
            .unwrap();
        assert!(unchanged);
    }

    #[test]
    fn copies_shared_acyclic_references_separately() {
        let lua = Lua::new();
        let value = eval(&lua, "local inner = {} return { inner, inner }");
        let copied = Transfer::from_lua(&value).unwrap().to_lua(&lua).unwrap();
        lua.globals().set("copied", copied).unwrap();
        let separate: bool = lua.load("return copied[1] ~= copied[2]").eval().unwrap();
        assert!(separate);
    }

    #[test]
    fn rejects_values_that_cannot_leave_their_vm() {
        let lua = Lua::new();
        assert!(rejection(&lua, "return function() end").contains("Functions"));
        assert!(rejection(&lua, "return coroutine.create(function() end)").contains("Threads"));
        assert!(rejection(&lua, "return { callback = print }").contains("Functions"));
        assert!(rejection(&lua, "local t = {} t.self = t return t").contains("Cyclic"));
        assert!(rejection(&lua, "return setmetatable({}, {})").contains("metatables"));
        assert!(rejection(&lua, "return { [{}] = true }").contains("table keys"));
        let userdata = LuaValue::UserData(lua.create_any_userdata(()).unwrap());
        let message = Transfer::from_lua(&userdata).unwrap_err().to_string();
        assert!(message.contains("Instances"));
    }

    #[test]
    fn rejects_overly_deep_tables() {
        let lua = Lua::new();
        let source = format!(
            "local root = {{}} local current = root for _ = 1, {} do local child = {{}} \
             current.child = child current = child end return root",
            MAX_DEPTH + 1
        );
        assert!(rejection(&lua, &source).contains("levels deep"));
    }
}
