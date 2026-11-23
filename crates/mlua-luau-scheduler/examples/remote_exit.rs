#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cargo_common_metadata)]

use std::{thread, time::Duration};

use async_io::block_on;
use futures_lite::future::pending;

use mlua::prelude::*;
use mlua_luau_scheduler::Scheduler;

pub fn main() -> LuaResult<()> {
    let lua = Lua::new();
    let sched = Scheduler::new(lua.clone());

    // A thread that waits forever keeps the scheduler busy until it is told to stop.
    lua.globals().set(
        "waitForever",
        lua.create_async_function(|_, ()| async move {
            pending::<()>().await;
            Ok(())
        })?,
    )?;
    sched.push_thread_front(lua.load("waitForever()"), ())?;

    // Another OS thread stops the idle scheduler through its thread-safe handle.
    let handle = sched.exit_handle();
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(25));
        handle.exit(5);
    });

    block_on(sched.run());
    stopper.join().expect("stopper thread panicked");

    assert_eq!(sched.get_exit_code(), Some(5));

    Ok(())
}

#[test]
fn test_remote_exit() -> LuaResult<()> {
    main()
}
