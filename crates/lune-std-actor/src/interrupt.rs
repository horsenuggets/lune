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

    The flag lives in thread-local storage because mlua already owns the VM's callback
    userdata slot. Each actor thread runs exactly one VM, and installing a second link on
    the same thread is refused, so the pointer always belongs to this thread's VM.
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
    // SAFETY: Luau allows raising errors from safepoint interrupts. Without stack space
    // for the message, the error is raised at a later safepoint instead, since the flag
    // stays set until the actor finishes.
    unsafe {
        if ffi::lua_checkstack(state, 1) == 0 {
            return;
        }
        ffi::lua_pushstring(state, TERMINATED_C_MESSAGE.as_ptr());
        ffi::lua_error(state);
    }
}

/**
    Makes Luau code in the given VM error once its actor is terminated.

    Must be called on the OS thread that runs the VM, since the check reads state that
    belongs to that thread.

    # Errors

    Errors if this OS thread already runs the VM of another actor, since one thread
    can only check the termination of one actor.
*/
pub(crate) fn install(lua: &Lua, link: Arc<ActorLink>) -> LuaResult<()> {
    debug_assert_eq!(TERMINATED_C_MESSAGE.to_str(), Ok(TERMINATED_MESSAGE));
    let already_attached = LINK.with(|slot| slot.borrow().is_some());
    if already_attached {
        return Err(LuaError::runtime(
            "This OS thread already runs the VM of an actor.",
        ));
    }
    FLAG.with(|flag| flag.set(link.terminated_flag()));
    LINK.with(|slot| *slot.borrow_mut() = Some(link));
    // SAFETY: Setting the interrupt only changes a callback pointer of this VM.
    unsafe {
        lua.exec_raw::<()>((), |state| {
            (*ffi::lua_callbacks(state)).interrupt = Some(interrupt);
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::transfer::Transfer;

    use super::*;

    #[test]
    fn refuses_a_second_actor_on_the_same_thread() {
        let first = Arc::new(ActorLink::new(Transfer::Nil));
        let second = Arc::new(ActorLink::new(Transfer::Nil));
        install(&Lua::new(), Arc::clone(&first)).unwrap();
        let message = install(&Lua::new(), Arc::clone(&second))
            .unwrap_err()
            .to_string();
        assert!(message.contains("already runs"), "{message}");
        // The first actor's flag still controls this thread.
        assert_eq!(FLAG.with(Cell::get), first.terminated_flag());
    }
}
