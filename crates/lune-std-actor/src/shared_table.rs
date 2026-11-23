use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ffi::c_void,
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock},
};

use mlua::prelude::*;

use lune_utils::TableBuilder;

use crate::transfer::MAX_DEPTH;

const CYCLE_ERROR: &str = "Storing this SharedTable would make it contain itself.";
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

/**
    Serializes table-valued writes with each other and with deep copies.

    A cycle can only appear when a write stores a table value, so holding this lock while
    checking reachability and writing means that two actors storing each other's tables at
    the same moment can never form a cycle together. Deep copies hold it across all their
    snapshots so concurrent edge reversals cannot create a cyclic copy. Writes of other
    values never take it.
*/
static STRUCTURE: Mutex<()> = Mutex::new(());

fn structure() -> MutexGuard<'static, ()> {
    STRUCTURE.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct SharedState {
    entries: HashMap<SharedKey, SharedValue>,
    version: u64,
}

struct SharedInner {
    frozen: bool,
    state: RwLock<SharedState>,
}

// Formatting never descends into nested tables, which could be arbitrarily deep.
impl fmt::Debug for SharedInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len();
        f.debug_struct("SharedTable")
            .field("entries", &entries)
            .field("frozen", &self.frozen)
            .finish()
    }
}

impl fmt::Debug for SharedState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedState")
            .field("entries", &self.entries.len())
            .field("version", &self.version)
            .finish()
    }
}

/*
    Dropping a table releases its nested tables one at a time instead of recursively, so
    tearing down an arbitrarily deep chain of shared tables can never overflow the stack.
    A nested table that other handles still use stays alive and is released by them.
*/
impl Drop for SharedInner {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        drain_tables(self, &mut pending);
        while let Some(SharedTable { inner }) = pending.pop() {
            if let Ok(mut nested) = Arc::try_unwrap(inner) {
                drain_tables(&mut nested, &mut pending);
            }
        }
    }
}

fn drain_tables(inner: &mut SharedInner, pending: &mut Vec<SharedTable>) {
    let state = inner
        .state
        .get_mut()
        .unwrap_or_else(PoisonError::into_inner);
    for (_, value) in state.entries.drain() {
        if let SharedValue::Table(table) = value {
            pending.push(table);
        }
    }
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

    /// Whether `target` is this table, or is nested anywhere inside it.
    fn reaches(&self, target: &Self) -> bool {
        let mut visited = HashSet::new();
        let mut pending = vec![self.clone()];
        while let Some(table) = pending.pop() {
            if table.is_same(target) {
                return true;
            }
            if !visited.insert(Arc::as_ptr(&table.inner)) {
                continue;
            }
            table.read(|state| {
                for value in state.entries.values() {
                    if let SharedValue::Table(nested) = value {
                        pending.push(nested.clone());
                    }
                }
            });
        }
        false
    }

    /**
        Takes the structure lock for a write that stores `value`, after checking that
        storing it cannot make this table contain itself. Writes of anything other than a
        table cannot create a cycle, so they take no lock.
    */
    fn guard_store(
        &self,
        value: Option<&SharedValue>,
    ) -> LuaResult<Option<MutexGuard<'static, ()>>> {
        let Some(SharedValue::Table(nested)) = value else {
            return Ok(None);
        };
        let guard = structure();
        if nested.reaches(self) {
            return Err(LuaError::runtime(CYCLE_ERROR));
        }
        #[cfg(test)]
        tests::after_cycle_check();
        Ok(Some(guard))
    }

    fn set(&self, key: SharedKey, value: Option<SharedValue>) -> LuaResult<()> {
        let _guard = self.guard_store(value.as_ref())?;
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

    /**
        Copies this table, and with `deep` every table nested inside it, one table at a
        time instead of recursively, so arbitrarily deep chains cannot overflow the stack.
        Deep copies hold the structure lock so concurrent edge reversals cannot combine
        snapshots from different acyclic graphs into a cyclic copy. Edge removals and
        scalar writes may still proceed, since neither can introduce a cycle.

        A table reached through several paths is copied once and shared the same way in
        the result, so the work grows with the number of distinct tables, not paths.

        Copies are found by the address of their source table, so every source table is
        kept alive until the copy finishes. Otherwise another actor could free a source
        that was already copied, and a new table at the same address would wrongly map
        to that earlier copy.
    */
    fn copy(&self, deep: bool, frozen: bool) -> LuaResult<Self> {
        let _guard = deep.then(structure);
        let root = Self::with_entries(HashMap::new(), frozen);
        let mut copies: HashMap<*const SharedInner, (Self, Self)> = HashMap::new();
        copies.insert(Arc::as_ptr(&self.inner), (self.clone(), root.clone()));
        let mut pending = vec![(self.clone(), root.clone())];
        while let Some((source, destination)) = pending.pop() {
            let mut entries = HashMap::new();
            for (key, value) in source.snapshot() {
                let value = match value {
                    SharedValue::Table(nested) if deep => {
                        let (_, copy) =
                            copies.entry(Arc::as_ptr(&nested.inner)).or_insert_with(|| {
                                let copy = Self::with_entries(HashMap::new(), frozen);
                                pending.push((nested.clone(), copy.clone()));
                                (nested, copy)
                            });
                        SharedValue::Table(copy.clone())
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
            // The copy is not shared with anyone yet, so even a frozen copy is filled here.
            destination
                .inner
                .state
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .entries = entries;
            drop((source, destination));
            #[cfg(test)]
            tests::after_copy_step();
        }
        Ok(root)
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
        let _guard = table.guard_store(next.as_ref())?;
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
    use std::{cell::RefCell, rc::Rc, sync::Condvar, thread, time::Duration};

    use super::*;

    /**
        A rendezvous that stores taking part in the race test wait at, right after their
        cycle check and before their write, until both have arrived. With the structure
        lock in place the second store cannot get past the lock, so the first gives up
        after a short timeout. Without the lock both checks pass before either write.
    */
    static RENDEZVOUS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());
    const RENDEZVOUS_TIMEOUT: Duration = Duration::from_millis(200);

    thread_local! {
        // Only the race test's own store threads take part, never other tests' threads.
        static IN_RACE: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn after_cycle_check() {
        if !IN_RACE.with(Cell::get) {
            return;
        }
        let (count, arrived) = &RENDEZVOUS;
        let mut count = count.lock().unwrap_or_else(PoisonError::into_inner);
        *count += 1;
        arrived.notify_all();
        let _ = arrived
            .wait_timeout_while(count, RENDEZVOUS_TIMEOUT, |count| *count < 2)
            .unwrap_or_else(PoisonError::into_inner);
    }

    /**
        A hook that runs after each table of a deep copy has been copied and its source
        handle dropped. The stale-address test uses it to free an already copied source
        table in the middle of a copy, the way another actor could.
    */
    type CopyStep = Box<dyn FnMut(usize)>;

    thread_local! {
        static COPY_STEP: RefCell<Option<(usize, CopyStep)>> = const { RefCell::new(None) };
    }

    pub(super) fn after_copy_step() {
        COPY_STEP.with(|hook| {
            if let Some((step, callback)) = hook.borrow_mut().as_mut() {
                *step += 1;
                callback(*step);
            }
        });
    }

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

    #[test]
    fn rejects_every_way_of_making_a_table_contain_itself() {
        let lua = library_lua();
        for source in [
            "local st = SharedTable.new() st.Self = st",
            "local a, b = SharedTable.new(), SharedTable.new() a.B = b b.A = a",
            "local a = SharedTable.new() local b = SharedTable.new({ C = SharedTable.new() }) a.B = b b.C.A = a",
            "local a = SharedTable.new() a.Plain = { Back = a }",
            "local a = SharedTable.new() SharedTable.update(a, 'Self', function() return a end)",
        ] {
            let message = lua.load(source).exec().unwrap_err().to_string();
            assert!(
                message.contains("contain itself"),
                "{source} gave {message}"
            );
        }
        // Sharing one table under several keys or parents is still allowed.
        let ok: bool = lua
            .load(
                r"
                    local shared = SharedTable.new({ Value = 1 })
                    local a, b = SharedTable.new(), SharedTable.new()
                    a.First, a.Second, b.Same = shared, shared, shared
                    local copy = SharedTable.clone(a, true)
                    return copy.First.Value == 1 and copy.First ~= shared and b.Same == shared
                ",
            )
            .eval()
            .unwrap();
        assert!(ok);
    }

    #[test]
    fn concurrent_cross_stores_never_form_a_cycle() {
        // Two actors store each other's tables at the same moment. At most one of the
        // two writes may succeed, otherwise each table would contain the other. The
        // rendezvous makes both stores reach the window between their cycle check and
        // their write together whenever nothing serializes them, so this is deterministic.
        let key = SharedKey::String(b"Other".to_vec());
        for _ in 0..3 {
            *RENDEZVOUS.0.lock().unwrap_or_else(PoisonError::into_inner) = 0;
            let a = SharedTable::with_entries(HashMap::new(), false);
            let b = SharedTable::with_entries(HashMap::new(), false);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let stores: Vec<_> = [(a.clone(), b.clone()), (b.clone(), a.clone())]
                .into_iter()
                .map(|(parent, child)| {
                    let barrier = Arc::clone(&barrier);
                    let key = key.clone();
                    thread::spawn(move || {
                        IN_RACE.with(|in_race| in_race.set(true));
                        barrier.wait();
                        parent.set(key, Some(SharedValue::Table(child))).is_ok()
                    })
                })
                .collect();
            let succeeded = stores
                .into_iter()
                .map(|store| store.join().unwrap())
                .filter(|ok| *ok)
                .count();
            assert_eq!(succeeded, 1, "exactly one cross store may succeed");
            assert!(!a.reaches(&b) || !b.reaches(&a));
            // Break the edge so the pair is released like any other tables.
            a.set(key.clone(), None).unwrap();
            b.set(key.clone(), None).unwrap();
        }
    }

    #[test]
    fn deep_clones_keep_copied_sources_alive_so_their_addresses_cannot_be_reused() {
        // Another actor releases a source table right after it was copied. Copies are found
        // by source address, so the clone must keep that table alive until it finishes.
        // Otherwise the freed address could be reused by a new table, which would then map
        // to the earlier, unrelated copy. A weak handle observes whether it stayed alive,
        // which does not depend on when the allocator reuses an address.
        let key = SharedKey::String(b"Child".to_vec());
        let root = SharedTable::with_entries(HashMap::new(), false);
        let child = SharedTable::with_entries(HashMap::new(), false);
        let weak_child = Arc::downgrade(&child.inner);
        root.set(key.clone(), Some(SharedValue::Table(child)))
            .unwrap();
        let alive = Rc::new(Cell::new(None));
        let hook_root = root.clone();
        let hook_alive = Rc::clone(&alive);
        COPY_STEP.with(|hook| {
            *hook.borrow_mut() = Some((
                0,
                Box::new(move |step| {
                    // Step one copies the root, step two copies the child.
                    if step != 2 {
                        return;
                    }
                    hook_root.set(key.clone(), None).unwrap();
                    hook_alive.set(Some(weak_child.upgrade().is_some()));
                }),
            ));
        });
        let copy = root.copy(true, false);
        COPY_STEP.with(|hook| hook.borrow_mut().take());
        assert!(copy.is_ok());
        assert_eq!(
            alive.get(),
            Some(true),
            "a copied source table must stay alive until the copy finishes"
        );
    }

    #[test]
    fn deep_clones_cannot_capture_cycles_from_concurrent_edge_reversals() {
        for frozen in [false, true] {
            let child_key = SharedKey::String(b"Child".to_vec());
            let back_key = SharedKey::String(b"Back".to_vec());
            let root = SharedTable::with_entries(HashMap::new(), false);
            let child = SharedTable::with_entries(HashMap::new(), false);
            root.set(child_key.clone(), Some(SharedValue::Table(child.clone())))
                .unwrap();
            let hook_root = root.clone();
            let hook_child = child.clone();
            let hook_child_key = child_key.clone();
            let hook_back_key = back_key.clone();
            let writer = Rc::new(RefCell::new(None));
            let hook_writer = Rc::clone(&writer);
            COPY_STEP.with(|hook| {
                *hook.borrow_mut() = Some((
                    0,
                    Box::new(move |step| {
                        if step != 1 {
                            return;
                        }
                        // The root snapshot already contains Child. Reverse that edge
                        // before the child's snapshot unless the deep copy blocks stores.
                        hook_root.set(hook_child_key.clone(), None).unwrap();
                        let root = hook_root.clone();
                        let child = hook_child.clone();
                        let back_key = hook_back_key.clone();
                        let (started, ready) = std::sync::mpsc::channel();
                        let (finished, completion) = std::sync::mpsc::channel();
                        *hook_writer.borrow_mut() = Some(thread::spawn(move || {
                            started.send(()).unwrap();
                            child.set(back_key, Some(SharedValue::Table(root))).unwrap();
                            let _ = finished.send(());
                        }));
                        ready.recv().unwrap();
                        let _ = completion.recv_timeout(RENDEZVOUS_TIMEOUT);
                    }),
                ));
            });
            let copy = root.copy(true, frozen);
            COPY_STEP.with(|hook| hook.borrow_mut().take());
            writer.borrow_mut().take().unwrap().join().unwrap();
            let copy = copy.unwrap();
            let Some(SharedValue::Table(copied_child)) =
                copy.read(|state| state.entries.get(&child_key).cloned())
            else {
                panic!("the root snapshot must preserve its child");
            };
            let copied_back = copied_child.read(|state| state.entries.get(&back_key).cloned());
            let cyclic =
                matches!(copied_back, Some(SharedValue::Table(back)) if back.is_same(&copy));
            // Also release an invalid frozen copy when testing without the fix.
            copied_child.inner.state.write().unwrap().entries.clear();
            child.set(back_key, None).unwrap();
            assert!(
                !cyclic,
                "a deep copy must not combine reversed edges into a cycle"
            );
            assert_eq!(copy.inner.frozen, frozen);
            assert_eq!(copied_child.inner.frozen, frozen);
        }
    }

    #[test]
    fn deep_clones_stay_acyclic_and_complete_while_other_threads_churn_tables() {
        // A sanity check under real concurrency. Other threads keep replacing the shared
        // structure and allocating and freeing tables while this thread deep-clones it.
        let key = SharedKey::String(b"Child".to_vec());
        let root = SharedTable::with_entries(HashMap::new(), false);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let churners: Vec<_> = (0..3)
            .map(|_| {
                let root = root.clone();
                let key = key.clone();
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let chain = SharedTable::with_entries(HashMap::new(), false);
                        let leaf = SharedTable::with_entries(HashMap::new(), false);
                        chain
                            .set(key.clone(), Some(SharedValue::Table(leaf)))
                            .unwrap();
                        root.set(key.clone(), Some(SharedValue::Table(chain)))
                            .unwrap();
                        drop(SharedTable::with_entries(HashMap::new(), false));
                    }
                })
            })
            .collect();
        for _ in 0..2_000 {
            let copy = root.copy(true, false).unwrap();
            assert!(!copy.reaches(&root), "a copy must never contain its source");
            let mut cursor = copy.clone();
            let mut depth = 0;
            while let Some(SharedValue::Table(next)) =
                cursor.read(|state| state.entries.get(&key).cloned())
            {
                assert!(!next.reaches(&cursor), "a copy must never contain a cycle");
                cursor = next;
                depth += 1;
                assert!(depth <= 2, "a copy must have the shape of one stored chain");
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for churner in churners {
            churner.join().unwrap();
        }
    }

    #[test]
    fn deep_chains_clone_format_and_drop_without_overflowing() {
        // A default test thread has a small stack, so recursion over this chain would
        // overflow and abort the process.
        thread::spawn(|| {
            let key = SharedKey::String(b"Child".to_vec());
            let root = SharedTable::with_entries(HashMap::new(), false);
            let mut current = root.clone();
            for _ in 0..200_000 {
                let child = SharedTable::with_entries(HashMap::new(), false);
                current
                    .set(key.clone(), Some(SharedValue::Table(child.clone())))
                    .unwrap();
                current = child;
            }
            current
                .set(key.clone(), Some(SharedValue::Number(7.0)))
                .unwrap();
            drop(current);
            let copy = root.copy(true, false).unwrap();
            assert!(!copy.is_same(&root));
            assert!(format!("{copy:?}").contains("entries"));
            // Walk the copy to its end to prove that every level was copied.
            let mut depth = 0;
            let mut cursor = copy.clone();
            loop {
                let next = cursor.read(|state| state.entries.get(&key).cloned());
                match next {
                    Some(SharedValue::Table(nested)) => {
                        cursor = nested;
                        depth += 1;
                    }
                    Some(SharedValue::Number(n)) => {
                        assert_eq!(n, 7.0);
                        break;
                    }
                    other => panic!("unexpected value {other:?}"),
                }
            }
            assert_eq!(depth, 200_000);
            drop(cursor);
            drop(copy);
            drop(root);
        })
        .join()
        .expect("deep chains must not overflow the stack");
    }

    #[test]
    fn deep_clones_copy_each_shared_table_once_and_keep_its_sharing() {
        // Every level references the next level twice, so the graph has 2^40 paths but
        // only 41 tables. Copying once per path would never finish in Rust, where
        // terminating an actor cannot stop it.
        const LEVELS: usize = 40;
        let (sender, receiver) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let first = SharedKey::String(b"First".to_vec());
            let second = SharedKey::String(b"Second".to_vec());
            let mut level = SharedTable::with_entries(HashMap::new(), false);
            level
                .set(first.clone(), Some(SharedValue::Number(1.0)))
                .unwrap();
            for _ in 0..LEVELS {
                let parent = SharedTable::with_entries(HashMap::new(), false);
                for key in [&first, &second] {
                    parent
                        .set(key.clone(), Some(SharedValue::Table(level.clone())))
                        .unwrap();
                }
                level = parent;
            }
            let copy = level.copy(true, false).unwrap();
            let mut source = level;
            let mut cursor = copy;
            for _ in 0..LEVELS {
                let (Some(SharedValue::Table(a)), Some(SharedValue::Table(b))) =
                    cursor.read(|state| {
                        (
                            state.entries.get(&first).cloned(),
                            state.entries.get(&second).cloned(),
                        )
                    })
                else {
                    panic!("every copied level must reference the next level twice");
                };
                assert!(
                    a.is_same(&b),
                    "shared references must stay shared in the copy"
                );
                let Some(SharedValue::Table(next_source)) =
                    source.read(|state| state.entries.get(&first).cloned())
                else {
                    panic!("every source level must reference the next level");
                };
                assert!(
                    !a.is_same(&next_source),
                    "a deep copy must not reuse source tables"
                );
                cursor = a;
                source = next_source;
            }
            let leaf = cursor.read(|state| state.entries.get(&first).cloned());
            sender
                .send(matches!(leaf, Some(SharedValue::Number(n)) if n == 1.0))
                .unwrap();
        });
        let finished = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("a deeply shared graph must copy quickly");
        assert!(finished, "the copy must keep the leaf value");
    }
}
