use std::{
    cell::{Cell, RefCell},
    ffi::c_int,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use mlua::{ffi, prelude::*};

use crate::link::{ActorLink, TERMINATED_MESSAGE};

const TERMINATED_C_MESSAGE: &std::ffi::CStr = c"The actor was terminated.";

thread_local! {
    // The link keeps the flag alive for as long as the actor's thread uses it.
    static LINK: RefCell<Option<Arc<ActorLink>>> = const { RefCell::new(None) };
    static FLAG: Cell<*const AtomicBool> = const { Cell::new(std::ptr::null()) };
}

/*
    Luau calls the interrupt at every safepoint, such as loop back edges and calls. The
    general-purpose interrupt support in mlua wraps each call in protected error handling,
    which slowed CPU-bound actors by about a quarter. This check only reads one atomic
    flag through a thread-local pointer, and raises an error once the actor is terminated.
*/
unsafe extern "C-unwind" fn interrupt(state: *mut ffi::lua_State, gc: c_int) {
    // Errors cannot be raised during garbage collection steps.
    if gc >= 0 {
        return;
    }
    let flag = FLAG.with(Cell::get);
    // SAFETY: The pointer comes from the link stored in this thread's LINK slot.
    if flag.is_null() || unsafe { !(*flag).load(Ordering::Relaxed) } {
        return;
    }
    // SAFETY: Luau allows raising errors from safepoint interrupts.
    unsafe {
        ffi::lua_checkstack(state, 1);
        ffi::lua_pushstring(state, TERMINATED_C_MESSAGE.as_ptr());
        ffi::lua_error(state);
    }
}

/**
    Makes Luau code in the given VM error once its actor is terminated.

    Must be called on the OS thread that runs the VM, since the check reads state that
    belongs to that thread.
*/
pub(crate) fn install(lua: &Lua, link: Arc<ActorLink>) {
    debug_assert_eq!(TERMINATED_C_MESSAGE.to_str(), Ok(TERMINATED_MESSAGE));
    FLAG.with(|flag| flag.set(link.terminated_flag()));
    LINK.with(|slot| *slot.borrow_mut() = Some(link));
    // SAFETY: Setting the interrupt only changes a callback pointer of this VM.
    let installed = unsafe {
        lua.exec_raw::<()>((), |state| {
            (*ffi::lua_callbacks(state)).interrupt = Some(interrupt);
        })
    };
    installed.expect("installing an interrupt callback cannot fail");
}
