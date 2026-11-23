#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cargo_common_metadata)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_io::{Timer, block_on};

use mlua::prelude::*;
use mlua_luau_scheduler::{LuaSpawnExt, Scheduler};

/// Runs a script that spawns panicking Rust tasks, returning the reported errors and the
/// result of the script's main thread.
fn run(script: &'static str) -> LuaResult<(Vec<String>, Option<LuaResult<LuaMultiValue>>)> {
    let lua = Lua::new();
    let sched = Scheduler::new(lua.clone());

    lua.globals().set(
        "detachSend",
        lua.create_function(|lua, ()| {
            lua.spawn(async { panic!("controlled detached panic") })
                .detach();
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "dropSend",
        lua.create_async_function(|lua, ()| async move {
            let task = lua.spawn(async { panic!("controlled dropped panic") });
            // Let the task panic before its handle is dropped without being awaited.
            Timer::after(Duration::from_millis(20)).await;
            drop(task);
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "spawnLocal",
        lua.create_function(|lua, ()| {
            lua.spawn_local(async { panic!("controlled local panic") });
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "awaitSend",
        lua.create_async_function(|lua, ()| async move {
            lua.spawn(async { panic!("controlled awaited panic") })
                .await;
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "pause",
        lua.create_async_function(|_, ()| async {
            Timer::after(Duration::from_millis(50)).await;
            Ok(())
        })?,
    )?;

    let errors = Arc::new(Mutex::new(Vec::new()));
    let errors_inner = Arc::clone(&errors);
    sched.set_error_callback(move |error| {
        errors_inner.lock().unwrap().push(error.to_string());
    });

    let id = sched.push_thread_back(lua.load(script), ())?;
    block_on(sched.run());

    let errors = errors.lock().unwrap().clone();
    Ok((errors, sched.get_thread_result(id)))
}

pub fn main() -> LuaResult<()> {
    // Detached and dropped tasks panic while the main thread keeps running and returns.
    // Each panic must still fail the run instead of being silently discarded.
    let (errors, result) = run("detachSend() dropSend() spawnLocal() pause() return 1")?;
    assert!(
        matches!(result, Some(Ok(_))),
        "the main thread itself succeeds"
    );
    assert_eq!(
        errors.len(),
        3,
        "every unobserved panic is reported once: {errors:?}"
    );
    for expected in ["detached", "dropped", "local"] {
        assert!(
            errors
                .iter()
                .any(|error| error.contains(&format!("controlled {expected} panic"))),
            "the {expected} panic must be reported: {errors:?}"
        );
    }

    // A panic that the awaiting thread observes fails that thread and is reported once.
    let (errors, result) = run("awaitSend() return 1")?;
    assert!(matches!(result, Some(Err(_))), "the awaiting thread fails");
    assert_eq!(
        errors.len(),
        1,
        "an awaited panic is not reported twice: {errors:?}"
    );
    assert!(
        errors[0].contains("controlled awaited panic"),
        "{}",
        errors[0]
    );

    Ok(())
}

#[test]
fn test_detached_panic() -> LuaResult<()> {
    main()
}
