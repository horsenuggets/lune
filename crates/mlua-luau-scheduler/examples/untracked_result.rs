#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cargo_common_metadata)]

use std::{
    panic,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_io::{Timer, block_on};
use futures_lite::future::yield_now;

use mlua::prelude::*;
use mlua_luau_scheduler::{LuaSchedulerExt, LuaSpawnExt, Scheduler};

const MAIN_SCRIPT: &str = include_str!("./lua/untracked_result.luau");

/**
    Tracks a thread, and then stops tracking it (by taking its result) while
    the thread is still suspended inside of an async function. The scheduler
    must discard the result once the thread finishes, instead of panicking.
*/
pub fn main() -> LuaResult<()> {
    // Panics inside of scheduler tasks are caught by the executor,
    // so we need a panic hook to find out if one happened at all
    let panicked = Arc::new(AtomicBool::new(false));
    let previous_hook = panic::take_hook();
    panic::set_hook({
        let panicked = Arc::clone(&panicked);
        Box::new(move |info| {
            panicked.store(true, Ordering::SeqCst);
            eprintln!("{info}");
        })
    });

    let lua = Lua::new();
    lua.globals().set(
        "sleep",
        lua.create_async_function(|_, duration: f64| async move {
            yield_now().await;
            Timer::after(Duration::from_secs_f64(duration)).await;
            Ok(())
        })?,
    )?;

    let sched = Scheduler::new(lua.clone());
    let thread = lua.load(MAIN_SCRIPT).into_function()?;
    // NOTE: Pushing a thread directly onto the scheduler also tracks it
    let id = sched.push_thread_back(thread, ())?;

    // NOTE: Tasks can only be spawned while the scheduler is running,
    // so the thread itself calls this right before it starts sleeping
    lua.globals().set(
        "untrackLater",
        lua.create_function(move |lua, ()| {
            lua.spawn_local({
                let lua = lua.clone();
                async move {
                    Timer::after(Duration::from_millis(20)).await;
                    assert!(lua.get_thread_result(id).is_none());
                }
            });
            Ok(())
        })?,
    )?;

    block_on(sched.run());
    panic::set_hook(previous_hook);

    assert!(
        !panicked.load(Ordering::SeqCst),
        "Scheduler panicked while storing the result of an untracked thread"
    );
    assert!(
        lua.globals().get::<bool>("finished")?,
        "Thread should still run to completion after being untracked"
    );

    Ok(())
}

#[test]
fn test_untracked_result() -> LuaResult<()> {
    main()
}
