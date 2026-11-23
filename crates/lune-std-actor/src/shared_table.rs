use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::{Arc, PoisonError, RwLock},
};

use mlua::prelude::*;

use lune_utils::TableBuilder;

use crate::transfer::MAX_DEPTH;

const FROZEN_ERROR: &str = "Attempt to modify a frozen SharedTable.";
const KEY_ERROR: &str = "SharedTable keys must be strings or finite numbers.";
const VALUE_ERROR: &str =
    "SharedTable values must be nil, booleans, numbers, vectors, strings, tables or SharedTables.";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SharedKey {
    Number(u64),
    String(Vec<u8>),
}

impl SharedKey {
    fn from_lua(value: &LuaValue) -> LuaResult<Self> {
        let number = match value {
            LuaValue::String(s) => return Ok(Self::String(s.as_bytes().to_vec())),
            LuaValue::Integer(i) => *i as f64,
            LuaValue::Number(n) => *n,
            _ => return Err(LuaError::runtime(KEY_ERROR)),
        };
        if !number.is_finite() {
            return Err(LuaError::runtime(KEY_ERROR));
        }
        // Negative and positive zero index the same Luau table slot.
        let normalized = if number == 0.0 { 0.0 } else { number };
        Ok(Self::Number(normalized.to_bits()))
    }

    fn to_lua(&self, lua: &Lua) -> LuaResult<LuaValue> {
        Ok(match self {
            Self::Number(bits) => LuaValue::Number(f64::from_bits(*bits)),
            Self::String(s) => LuaValue::String(lua.create_string(s)?),
        })
    }
}

#[derive(Debug, Clone)]
enum SharedValue {
    Boolean(bool),
    Number(f64),
    Vector(LuaVector),
    String(Vec<u8>),
    Table(SharedTable),
}

impl SharedValue {
    /// Converts a Luau value, turning plain tables into new shared tables.
    fn from_lua(
        value: &LuaValue,
        visiting: &mut HashSet<*const c_void>,
    ) -> LuaResult<Option<Self>> {
        Ok(Some(match value {
            LuaValue::Nil => return Ok(None),
            LuaValue::Boolean(b) => Self::Boolean(*b),
            LuaValue::Integer(i) => Self::Number(*i as f64),
            LuaValue::Number(n) => Self::Number(*n),
            LuaValue::Vector(v) => Self::Vector(*v),
            LuaValue::String(s) => Self::String(s.as_bytes().to_vec()),
            LuaValue::Table(t) => Self::Table(SharedTable::from_table(t, false, visiting)?),
            LuaValue::UserData(ud) => match ud.borrow::<SharedTable>() {
                Ok(shared) => Self::Table(shared.clone()),
                Err(_) => return Err(LuaError::runtime(VALUE_ERROR)),
            },
            _ => return Err(LuaError::runtime(VALUE_ERROR)),
        }))
    }

    fn to_lua(&self, lua: &Lua) -> LuaResult<LuaValue> {
        Ok(match self {
            Self::Boolean(b) => LuaValue::Boolean(*b),
            Self::Number(n) => LuaValue::Number(*n),
            Self::Vector(v) => LuaValue::Vector(*v),
            Self::String(s) => LuaValue::String(lua.create_string(s)?),
            Self::Table(t) => LuaValue::UserData(lua.create_userdata(t.clone())?),
        })
    }
}

fn option_to_lua(value: Option<&SharedValue>, lua: &Lua) -> LuaResult<LuaValue> {
    match value {
        Some(value) => value.to_lua(lua),
        None => Ok(LuaValue::Nil),
    }
}

#[derive(Debug, Default)]
struct SharedState {
    entries: HashMap<SharedKey, SharedValue>,
    version: u64,
}

#[derive(Debug)]
struct SharedInner {
    frozen: bool,
    state: RwLock<SharedState>,
}

/**
    A table that every actor can read and update at the same time, like Roblox's
    `SharedTable`.

    Sending a shared table to another actor shares it instead of copying it. Each read
    and write is atomic, and every update is visible to all actors immediately.
*/
#[derive(Debug, Clone)]
pub struct SharedTable {
    inner: Arc<SharedInner>,
}

impl SharedTable {
    fn with_entries(entries: HashMap<SharedKey, SharedValue>, frozen: bool) -> Self {
        Self {
            inner: Arc::new(SharedInner {
                frozen,
                state: RwLock::new(SharedState {
                    entries,
                    version: 0,
                }),
            }),
        }
    }

    fn from_table(
        table: &LuaTable,
        frozen: bool,
        visiting: &mut HashSet<*const c_void>,
    ) -> LuaResult<Self> {
        if visiting.len() >= MAX_DEPTH {
            return Err(LuaError::runtime(format!(
                "Tables nested more than {MAX_DEPTH} levels deep cannot become SharedTables."
            )));
        }
        if table.metatable().is_some() {
            return Err(LuaError::runtime(
                "Tables with metatables cannot become SharedTables.",
            ));
        }
        let pointer = table.to_pointer();
        if !visiting.insert(pointer) {
            return Err(LuaError::runtime(
                "Cyclic tables cannot become SharedTables.",
            ));
        }
        let mut entries = HashMap::new();
        for pair in table.pairs::<LuaValue, LuaValue>() {
            let (key, value) = pair?;
            if let Some(value) = SharedValue::from_lua(&value, visiting)? {
                entries.insert(SharedKey::from_lua(&key)?, value);
            }
        }
        visiting.remove(&pointer);
        Ok(Self::with_entries(entries, frozen))
    }

    fn read<T>(&self, f: impl FnOnce(&SharedState) -> T) -> T {
        let state = self
            .inner
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        f(&state)
    }

    fn write<T>(&self, f: impl FnOnce(&mut SharedState) -> LuaResult<T>) -> LuaResult<T> {
        if self.inner.frozen {
            return Err(LuaError::runtime(FROZEN_ERROR));
        }
        let mut state = self
            .inner
            .state
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        f(&mut state)
    }

    fn set(&self, key: SharedKey, value: Option<SharedValue>) -> LuaResult<()> {
        self.write(|state| {
            match value {
                Some(value) => state.entries.insert(key, value),
                None => state.entries.remove(&key),
            };
            state.version += 1;
            Ok(())
        })
    }

    fn snapshot(&self) -> Vec<(SharedKey, SharedValue)> {
        self.read(|state| {
            state
                .entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
    }

    fn copy(&self, deep: bool, frozen: bool) -> LuaResult<Self> {
        let mut entries = HashMap::new();
        for (key, value) in self.snapshot() {
            let value = match value {
                SharedValue::Table(nested) if deep => {
                    SharedValue::Table(nested.copy(true, frozen)?)
                }
                SharedValue::Table(nested) if frozen && !nested.inner.frozen => {
                    return Err(LuaError::runtime(
                        "SharedTable.cloneAndFreeze needs nested SharedTables to be frozen unless deep is true.",
                    ));
                }
                other => other,
            };
            entries.insert(key, value);
        }
        Ok(Self::with_entries(entries, frozen))
    }

    /// Whether both handles refer to the same shared table.
    #[must_use]
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// The number of entries currently stored in the table.
    #[must_use]
    pub fn size(&self) -> usize {
        self.read(|state| state.entries.len())
    }
}

impl LuaUserData for SharedTable {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "SharedTable");
    }

    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::Index, |lua, this, key: LuaValue| {
            let key = SharedKey::from_lua(&key)?;
            let value = this.read(|state| state.entries.get(&key).cloned());
            option_to_lua(value.as_ref(), lua)
        });
        methods.add_meta_method(
            LuaMetaMethod::NewIndex,
            |_, this, (key, value): (LuaValue, LuaValue)| {
                let key = SharedKey::from_lua(&key)?;
                let value = SharedValue::from_lua(&value, &mut HashSet::new())?;
                this.set(key, value)
            },
        );
        methods.add_meta_method(LuaMetaMethod::Iter, |lua, this, ()| {
            // Iteration walks a snapshot, so concurrent updates never invalidate it.
            let entries = this.snapshot();
            let index = Cell::new(0);
            lua.create_function(move |lua, _: LuaMultiValue| {
                let current = index.get();
                index.set(current + 1);
                match entries.get(current) {
                    Some((key, value)) => Ok((key.to_lua(lua)?, value.to_lua(lua)?)),
                    None => Ok((LuaValue::Nil, LuaValue::Nil)),
                }
            })
        });
        methods.add_meta_method(LuaMetaMethod::Eq, |_, this, other: LuaAnyUserData| {
            Ok(other
                .borrow::<SharedTable>()
                .is_ok_and(|other| this.is_same(&other)))
        });
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| Ok("SharedTable"));
    }
}

fn borrow(value: &LuaAnyUserData) -> LuaResult<SharedTable> {
    value
        .borrow::<SharedTable>()
        .map(|table| table.clone())
        .map_err(|_| LuaError::runtime("Expected a SharedTable."))
}

fn shared_new(lua: &Lua, value: Option<LuaTable>) -> LuaResult<LuaAnyUserData> {
    let table = match value {
        Some(table) => SharedTable::from_table(&table, false, &mut HashSet::new())?,
        None => SharedTable::with_entries(HashMap::new(), false),
    };
    lua.create_userdata(table)
}

fn shared_clear(_: &Lua, table: LuaAnyUserData) -> LuaResult<()> {
    borrow(&table)?.write(|state| {
        state.entries.clear();
        state.version += 1;
        Ok(())
    })
}

fn shared_clone(
    lua: &Lua,
    (table, deep): (LuaAnyUserData, Option<bool>),
) -> LuaResult<LuaAnyUserData> {
    lua.create_userdata(borrow(&table)?.copy(deep.unwrap_or(false), false)?)
}

fn shared_clone_and_freeze(
    lua: &Lua,
    (table, deep): (LuaAnyUserData, Option<bool>),
) -> LuaResult<LuaAnyUserData> {
    lua.create_userdata(borrow(&table)?.copy(deep.unwrap_or(false), true)?)
}

fn shared_increment(
    _: &Lua,
    (table, key, delta): (LuaAnyUserData, LuaValue, f64),
) -> LuaResult<f64> {
    let key = SharedKey::from_lua(&key)?;
    borrow(&table)?.write(|state| match state.entries.get(&key) {
        Some(SharedValue::Number(previous)) => {
            let previous = *previous;
            state
                .entries
                .insert(key, SharedValue::Number(previous + delta));
            state.version += 1;
            Ok(previous)
        }
        _ => Err(LuaError::runtime(
            "SharedTable.increment needs an existing number value at the given key.",
        )),
    })
}

fn shared_is_frozen(_: &Lua, table: LuaAnyUserData) -> LuaResult<bool> {
    Ok(borrow(&table)?.inner.frozen)
}

fn shared_size(_: &Lua, table: LuaAnyUserData) -> LuaResult<usize> {
    Ok(borrow(&table)?.size())
}

fn shared_update(
    lua: &Lua,
    (table, key, callback): (LuaAnyUserData, LuaValue, LuaFunction),
) -> LuaResult<()> {
    let table = borrow(&table)?;
    let key = SharedKey::from_lua(&key)?;
    if table.inner.frozen {
        return Err(LuaError::runtime(FROZEN_ERROR));
    }
    // The callback runs without holding the lock, so it may read this table freely.
    // A concurrent write in the meantime makes the update retry with the newer value.
    loop {
        let (version, current) =
            table.read(|state| (state.version, state.entries.get(&key).cloned()));
        let next: LuaValue = callback.call(option_to_lua(current.as_ref(), lua)?)?;
        let next = SharedValue::from_lua(&next, &mut HashSet::new())?;
        let applied = table.write(|state| {
            if state.version != version {
                return Ok(false);
            }
            match next {
                Some(value) => state.entries.insert(key.clone(), value),
                None => state.entries.remove(&key),
            };
            state.version += 1;
            Ok(true)
        })?;
        if applied {
            return Ok(());
        }
    }
}

/**
    Creates the `SharedTable` library table.

    # Errors

    Errors when out of memory.
*/
pub(crate) fn library(lua: Lua) -> LuaResult<LuaTable> {
    TableBuilder::new(lua)?
        .with_function("clear", shared_clear)?
        .with_function("clone", shared_clone)?
        .with_function("cloneAndFreeze", shared_clone_and_freeze)?
        .with_function("increment", shared_increment)?
        .with_function("isFrozen", shared_is_frozen)?
        .with_function("new", shared_new)?
        .with_function("size", shared_size)?
        .with_function("update", shared_update)?
        .build_readonly()
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Counters hold exact integers, so exact comparisons are intended
mod tests {
    use std::thread;

    use super::*;

    fn library_lua() -> Lua {
        let lua = Lua::new();
        let library = library(lua.clone()).unwrap();
        lua.globals().set("SharedTable", library).unwrap();
        lua
    }

    #[test]
    fn converts_nested_tables_and_rejects_invalid_contents() {
        let lua = library_lua();
        let ok: bool = lua
            .load(
                r#"
                    local st = SharedTable.new({ Count = 1, Nested = { Name = "Inner" } })
                    return typeof(st.Nested) == "SharedTable" and st.Nested.Name == "Inner"
                        and SharedTable.size(st) == 2
                "#,
            )
            .eval()
            .unwrap();
        assert!(ok);
        for source in [
            "SharedTable.new()[true] = 1",
            "SharedTable.new().Value = print",
            "local t = {} t.t = t SharedTable.new(t)",
            "SharedTable.new()[0 / 0] = 1",
        ] {
            assert!(lua.load(source).exec().is_err(), "{source} must fail");
        }
    }

    #[test]
    fn frozen_tables_reject_writes() {
        let lua = library_lua();
        let message = lua
            .load(
                r"
                    local frozen = SharedTable.cloneAndFreeze(SharedTable.new({ Value = 1 }))
                    assert(SharedTable.isFrozen(frozen) and frozen.Value == 1)
                    frozen.Value = 2
                ",
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(message.contains("frozen"));
    }

    #[test]
    fn concurrent_increments_are_atomic() {
        let table = SharedTable::with_entries(
            HashMap::from([(
                SharedKey::String(b"Count".to_vec()),
                SharedValue::Number(0.0),
            )]),
            false,
        );
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let table = table.clone();
                thread::spawn(move || {
                    let lua = library_lua();
                    lua.globals().set("st", table).unwrap();
                    lua.load(
                        r#"
                            for _ = 1, 1000 do
                                SharedTable.increment(st, "Count", 1)
                                SharedTable.update(st, "Updated", function(value)
                                    return (value or 0) + 1
                                end)
                            end
                        "#,
                    )
                    .exec()
                    .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let key = |name: &str| SharedKey::String(name.as_bytes().to_vec());
        let value = |name: &str| table.read(|state| state.entries.get(&key(name)).cloned());
        assert!(matches!(value("Count"), Some(SharedValue::Number(n)) if n == 8000.0));
        assert!(matches!(value("Updated"), Some(SharedValue::Number(n)) if n == 8000.0));
    }
}
