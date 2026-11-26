#![allow(clippy::inline_always)]

use std::{cell::RefCell, rc::Rc};

use mlua::prelude::*;
use rustc_hash::FxHashMap;

use crate::events::{OnceEvent, OnceListener};

use super::id::ThreadId;

struct ThreadEvent {
    result: Option<LuaResult<LuaMultiValue>>,
    event: OnceEvent,
    // Keep the pointer-based identity alive until its result is consumed.
    thread: Option<LuaThread>,
}

impl ThreadEvent {
    fn new() -> Self {
        Self {
            result: None,
            event: OnceEvent::new(),
            thread: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ThreadMap {
    inner: Rc<RefCell<FxHashMap<ThreadId, ThreadEvent>>>,
}

impl ThreadMap {
    pub fn new() -> Self {
        let inner = Rc::new(RefCell::new(FxHashMap::default()));
        Self { inner }
    }

    #[inline(always)]
    pub fn track(&self, id: ThreadId) {
        self.inner.borrow_mut().insert(id, ThreadEvent::new());
    }

    #[inline(always)]
    pub fn is_tracked(&self, id: ThreadId) -> bool {
        self.inner.borrow().contains_key(&id)
    }

    #[inline(always)]
    pub fn insert(&self, thread: &LuaThread, result: LuaResult<LuaMultiValue>) {
        let id = ThreadId::from(thread);
        if let Some(tracker) = self.inner.borrow_mut().get_mut(&id) {
            tracker.thread = Some(thread.clone());
            tracker.result.replace(result);
            tracker.event.notify();
        } else {
            panic!("Thread must be tracked");
        }
    }

    #[inline(always)]
    pub fn listen(&self, id: ThreadId) -> OnceListener {
        if let Some(tracker) = self.inner.borrow().get(&id) {
            tracker.event.listen()
        } else {
            panic!("Thread must be tracked");
        }
    }

    #[inline(always)]
    pub fn remove(&self, id: ThreadId) -> Option<LuaResult<LuaMultiValue>> {
        if let Some(mut tracker) = self.inner.borrow_mut().remove(&id) {
            tracker.result.take()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_result_survives_coroutine_allocations() -> LuaResult<()> {
        use crate::{Functions, Scheduler};

        let lua = Lua::new();
        let scheduler = Scheduler::new(lua.clone());
        let id = scheduler.push_thread_back(lua.load("return 42"), ())?;
        async_io::block_on(scheduler.run());
        lua.gc_collect()?;
        lua.gc_collect()?;
        let functions = Functions::new(lua.clone())?;
        let empty = lua.load("").into_function()?;
        let mut allocated = Vec::new();
        for _ in 0..10000 {
            let thread = lua.create_thread(empty.clone())?;
            functions.spawn.call::<LuaThread>(thread.clone())?;
            allocated.push(thread);
        }
        let result = scheduler.get_thread_result(id).expect("tracked result")?;
        assert_eq!(result.len(), 1);
        assert_eq!(result.front(), Some(&LuaValue::Integer(42)));
        assert!(scheduler.get_thread_result(id).is_none());
        Ok(())
    }

    #[test]
    fn synchronous_completions_retain_success_and_error_identities() -> LuaResult<()> {
        use crate::{Functions, LuaSchedulerExt, Scheduler};

        for spawn in [false, true] {
            for fail in [false, true] {
                let lua = Lua::new();
                let scheduler = Scheduler::new(lua.clone());
                scheduler.remove_error_callback();
                let functions = Functions::new(lua.clone())?;
                let weak = lua.create_table()?;
                weak.set_metatable(Some(lua.create_table_from([("__mode", "v")])?))?;
                let source = if fail {
                    "error('expected failure')"
                } else {
                    "return 42"
                };
                let thread = lua.create_thread(lua.load(source).into_function()?)?;
                let id = ThreadId::from(&thread);
                weak.set(1, thread.clone())?;
                lua.track_thread(id);
                if spawn {
                    functions.spawn.call::<LuaThread>(thread.clone())?;
                } else {
                    let returned = functions.resume.call::<LuaMultiValue>(thread.clone())?;
                    assert_eq!(returned.front(), Some(&LuaValue::Boolean(!fail)));
                }
                drop(thread);
                lua.gc_collect()?;
                lua.gc_collect()?;
                assert!(matches!(weak.get::<LuaValue>(1)?, LuaValue::Thread(_)));
                let result = lua.get_thread_result(id).expect("tracked result");
                assert_eq!(result.is_err(), fail);
                if let Ok(values) = &result {
                    assert_eq!(values.front(), Some(&LuaValue::Integer(42)));
                }
                drop(result);
                assert!(lua.get_thread_result(id).is_none());
                lua.gc_collect()?;
                lua.gc_collect()?;
                assert!(weak.get::<LuaValue>(1)?.is_nil());
            }
        }
        Ok(())
    }

    #[test]
    fn completed_result_keeps_its_coroutine_identity_alive() -> LuaResult<()> {
        let lua = Lua::new();
        let weak = lua.create_table()?;
        let metatable = lua.create_table_from([("__mode", "v")])?;
        weak.set_metatable(Some(metatable))?;
        let thread = lua.create_thread(lua.load("return 42").into_function()?)?;
        let id = ThreadId::from(&thread);
        weak.set(1, thread.clone())?;
        let map = ThreadMap::new();
        map.track(id);
        map.insert(&thread, Ok(thread.resume::<LuaMultiValue>(())?));
        drop(thread);
        lua.gc_collect()?;
        lua.gc_collect()?;
        assert!(
            matches!(weak.get::<LuaValue>(1)?, LuaValue::Thread(_)),
            "An unread result must keep its coroutine identity alive"
        );
        let result = map.remove(id).expect("tracked result")?;
        assert_eq!(result.front(), Some(&LuaValue::Integer(42)));
        lua.gc_collect()?;
        lua.gc_collect()?;
        assert!(weak.get::<LuaValue>(1)?.is_nil());
        Ok(())
    }
}
