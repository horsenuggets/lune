#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cargo_common_metadata)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_io::{Timer, block_on};
use futures_lite::FutureExt;

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
            let (started, running) = async_channel::bounded(1);
            let task = lua.spawn(async move {
                started.try_send(()).unwrap();
                panic!("controlled dropped panic");
            });
            // Cancellation before the first poll legitimately prevents any panic.
            running.recv().await.map_err(LuaError::external)?;
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
    let (reported, reports) = async_channel::unbounded();
    lua.globals().set(
        "waitForErrors",
        lua.create_async_function(move |_, count: usize| {
            let reports = reports.clone();
            async move {
                async move {
                    for _ in 0..count {
                        reports.recv().await.map_err(LuaError::external)?;
                    }
                    Ok(())
                }
                .or(async {
                    // Bound a missing-report failure, not the tasks' scheduling latency.
                    Timer::after(Duration::from_secs(10)).await;
                    Err(LuaError::runtime(
                        "The expected panic reports never arrived.",
                    ))
                })
                .await
            }
        })?,
    )?;

    let errors = Arc::new(Mutex::new(Vec::new()));
    let errors_inner = Arc::clone(&errors);
    sched.set_error_callback(move |error| {
        errors_inner.lock().unwrap().push(error.to_string());
        reported.try_send(()).unwrap();
    });

    let id = sched.push_thread_back(lua.load(script), ())?;
    block_on(sched.run());

    let errors = errors.lock().unwrap().clone();
    Ok((errors, sched.get_thread_result(id)))
}

pub fn main() -> LuaResult<()> {
    // Detached and dropped tasks panic while the main thread keeps running and returns.
    // Each panic must still fail the run instead of being silently discarded.
    let (errors, result) = run("detachSend() dropSend() spawnLocal() waitForErrors(3) return 1")?;
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
