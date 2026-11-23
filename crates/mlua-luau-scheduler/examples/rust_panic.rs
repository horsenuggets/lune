#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cargo_common_metadata)]

use std::sync::{Arc, Mutex};

use async_io::block_on;

use mlua::prelude::*;
use mlua_luau_scheduler::Scheduler;

pub fn main() -> LuaResult<()> {
    let lua = Lua::new();
    let sched = Scheduler::new(lua.clone());

    // A Rust function that panics while a scheduled thread calls it.
    lua.globals().set(
        "explode",
        lua.create_function(|_, ()| -> LuaResult<()> { panic!("controlled panic") })?,
    )?;

    let errors = Arc::new(Mutex::new(Vec::new()));
    let errors_inner = Arc::clone(&errors);
    sched.set_error_callback(move |error| {
        errors_inner.lock().unwrap().push(error.to_string());
    });

    let id = sched.push_thread_back(lua.load("explode()"), ())?;
    block_on(sched.run());

    // The panic must surface as an error of the thread, never as a silent success.
    let errors = errors.lock().unwrap();
    assert_eq!(errors.len(), 1, "the panic must be reported once");
    assert!(errors[0].contains("controlled panic"), "{}", errors[0]);
    assert!(
        matches!(sched.get_thread_result(id), Some(Err(_))),
        "the thread must finish with an error"
    );

    Ok(())
}

#[test]
fn test_rust_panic() -> LuaResult<()> {
    main()
}
