#![allow(clippy::cargo_common_metadata)]

use std::{
    io::{IsTerminal, stderr, stdout},
    sync::{Arc, LazyLock},
};

use mlua::prelude::*;
use mlua_luau_scheduler::LuaSpawnExt;

use async_lock::Mutex as AsyncMutex;
use blocking::Unblock;
use futures_lite::prelude::*;

use lune_utils::{
    TableBuilder,
    actor::is_actor_worker,
    fmt::{ValueFormatConfig, pretty_format_multi_value},
};

mod prompt;
mod style_and_color;
mod terminal;
#[cfg(unix)]
mod terminal_replies;

use self::prompt::{PromptOptions, PromptResult, prompt};
use self::style_and_color::{ColorKind, StyleKind};

const FORMAT_CONFIG: ValueFormatConfig = ValueFormatConfig::new()
    .with_max_depth(4)
    .with_colors_enabled(false);

static INPUT_GATE: LazyLock<Arc<AsyncMutex<()>>> = LazyLock::new(|| Arc::new(AsyncMutex::new(())));
static REDIRECTED_INPUT_GATE: LazyLock<Arc<AsyncMutex<()>>> =
    LazyLock::new(|| Arc::new(AsyncMutex::new(())));

#[cfg(not(unix))]
static STDIN: LazyLock<Arc<AsyncMutex<futures_lite::io::BufReader<Unblock<std::io::Stdin>>>>> =
    LazyLock::new(|| {
        Arc::new(AsyncMutex::new(futures_lite::io::BufReader::new(
            Unblock::new(std::io::stdin()),
        )))
    });

const TYPEDEFS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/types.d.luau"));

/**
    Returns a string containing type definitions for the `stdio` standard library.
*/
#[must_use]
pub fn typedefs() -> String {
    TYPEDEFS.to_string()
}

/**
    Creates the `stdio` standard library module.

    # Errors

    Errors when out of memory.
*/
pub fn module(lua: Lua) -> LuaResult<LuaTable> {
    TableBuilder::new(lua)?
        .with_function("color", stdio_color)?
        .with_function("style", stdio_style)?
        .with_function("format", stdio_format)?
        .with_async_function("write", stdio_write)?
        .with_async_function("ewrite", stdio_ewrite)?
        .with_async_function("readLine", stdio_read_line)?
        .with_async_function("readToEnd", stdio_read_to_end)?
        .with_async_function("prompt", stdio_prompt)?
        .with_async_function("terminalCapabilities", stdio_terminal_capabilities)?
        .build_readonly()
}

fn stdio_color(lua: &Lua, color: ColorKind) -> LuaResult<LuaValue> {
    color.ansi_escape_sequence().into_lua(lua)
}

fn stdio_style(lua: &Lua, style: StyleKind) -> LuaResult<LuaValue> {
    style.ansi_escape_sequence().into_lua(lua)
}

fn stdio_format(_: &Lua, args: LuaMultiValue) -> LuaResult<String> {
    Ok(pretty_format_multi_value(&args, &FORMAT_CONFIG))
}

async fn stdio_write(_: Lua, s: LuaString) -> LuaResult<()> {
    let mut stdout = Unblock::new(stdout());
    stdout.write_all(&s.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

async fn stdio_ewrite(_: Lua, s: LuaString) -> LuaResult<()> {
    let mut stderr = Unblock::new(stderr());
    stderr.write_all(&s.as_bytes()).await?;
    stderr.flush().await?;
    Ok(())
}

fn input_gate(terminal: bool) -> &'static Arc<AsyncMutex<()>> {
    if terminal {
        &INPUT_GATE
    } else {
        &REDIRECTED_INPUT_GATE
    }
}

async fn acquire_input(
    cancellation: &terminal::InputCancellation,
    gate: &'static Arc<AsyncMutex<()>>,
    lua: &Lua,
    on_cancel: impl FnOnce() + 'static,
) -> LuaResult<async_lock::MutexGuardArc<()>> {
    use std::{cell::RefCell, rc::Rc, task::Poll};

    let acquisition = Rc::new(RefCell::new(Some(Box::pin(gate.lock_arc()))));
    let waiting = Rc::clone(&acquisition);
    cancellation.monitor(lua, move || {
        // A closed coroutine can retain its mutex waiter. Drop it independently,
        // but leave any already-running worker's input ownership untouched.
        waiting.borrow_mut().take();
        on_cancel();
    });
    std::future::poll_fn(|context| {
        let mut acquisition = acquisition.borrow_mut();
        let Some(waiting) = acquisition.as_mut() else {
            return Poll::Ready(Err(LuaError::runtime(
                "Standard input operation was cancelled.",
            )));
        };
        let result = waiting.as_mut().poll(context);
        if result.is_ready() {
            acquisition.take();
        }
        result.map(Ok)
    })
    .await
}

#[cfg(unix)]
async fn read_unix(lua: Lua, to_end: bool) -> LuaResult<LuaString> {
    use std::sync::{Mutex, atomic::Ordering};

    let gate = input_gate(std::io::stdin().is_terminal());
    let cancellation = terminal::InputCancellation::default();
    let cancelled = cancellation.token();
    let delivery = Arc::new(Mutex::new(None));
    let abandoned = Arc::clone(&delivery);
    let input = acquire_input(&cancellation, gate, &lua, move || {
        abandoned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    })
    .await?;
    let worker_delivery = Arc::clone(&delivery);
    lua.spawn_blocking(move || {
        let mut transaction = terminal::input_transaction(Arc::clone(&cancelled));
        let result = if to_end {
            terminal::read_to_end(&mut transaction)
        } else {
            terminal::read_line(&mut transaction).map(String::into_bytes)
        };
        // Tuple fields drop in order, rolling back bytes before releasing ownership.
        let completed = (transaction, result, input);
        let mut delivery = worker_delivery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !cancelled.load(Ordering::Acquire) {
            *delivery = Some(completed);
        }
    })
    .await;
    let mut completed = delivery
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .ok_or_else(|| LuaError::runtime("Standard input read was cancelled."))?;
    // Both successful input and errors are now delivered to the resumed caller.
    // Only abandoned results should restore bytes for a subsequent read.
    completed.0.commit();
    lua.create_string(completed.1.into_lua_err()?)
}

#[cfg(unix)]
async fn stdio_read_line(lua: Lua, (): ()) -> LuaResult<LuaString> {
    read_unix(lua, false).await
}

#[cfg(not(unix))]
async fn stdio_read_line(lua: Lua, (): ()) -> LuaResult<LuaString> {
    let cancellation = terminal::InputCancellation::default();
    let gate = input_gate(std::io::stdin().is_terminal());
    let _input = acquire_input(&cancellation, gate, &lua, || {}).await?;
    let mut string = String::new();
    STDIN.lock_arc().await.read_line(&mut string).await?;
    lua.create_string(&string)
}

#[cfg(unix)]
async fn stdio_read_to_end(lua: Lua, (): ()) -> LuaResult<LuaString> {
    read_unix(lua, true).await
}

#[cfg(not(unix))]
async fn stdio_read_to_end(lua: Lua, (): ()) -> LuaResult<LuaString> {
    let cancellation = terminal::InputCancellation::default();
    let gate = input_gate(std::io::stdin().is_terminal());
    let _input = acquire_input(&cancellation, gate, &lua, || {}).await?;
    let mut buffer = Vec::new();
    STDIN.lock_arc().await.read_to_end(&mut buffer).await?;
    lua.create_string(&buffer)
}

async fn stdio_prompt(lua: Lua, options: PromptOptions) -> LuaResult<PromptResult> {
    // Actors run concurrently with the main runtime and would fight over the terminal.
    if is_actor_worker(&lua) {
        return Err(LuaError::runtime(
            "stdio.prompt is unavailable inside an actor, since actors cannot share the terminal.",
        ));
    }
    let cancellation = terminal::InputCancellation::default();
    let cancelled = cancellation.token();
    let input = acquire_input(&cancellation, &INPUT_GATE, &lua, || {}).await?;
    lua.spawn_blocking(move || {
        // Keep ownership in the worker even if its awaiting Luau task is cancelled.
        let _input = input;
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(LuaError::runtime("Standard input prompt was cancelled."));
        }
        prompt(options)
    })
    .await
    .into_lua_err()
}

async fn stdio_terminal_capabilities(lua: Lua, (): ()) -> LuaResult<LuaTable> {
    let supported = if is_actor_worker(&lua) {
        false
    } else if let Some(supported) = terminal::cached_foreground_opacity() {
        supported
    } else if let Some(input) = INPUT_GATE.try_lock_arc() {
        lua.spawn_blocking(move || {
            let _input = input;
            terminal::foreground_opacity()
        })
        .await
    } else {
        false
    };
    TableBuilder::new(lua)?
        .with_value("ForegroundOpacity", supported)?
        .build_readonly()
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use mlua_luau_scheduler::Scheduler;

    #[test]
    fn input_sources_select_shared_console_or_redirected_ownership() {
        assert!(Arc::ptr_eq(input_gate(true), &INPUT_GATE));
        assert!(Arc::ptr_eq(input_gate(false), &REDIRECTED_INPUT_GATE));
        assert!(!Arc::ptr_eq(input_gate(true), input_gate(false)));
    }

    #[test]
    fn console_reads_exclude_prompts_but_redirected_reads_do_not() {
        for console in [false, true] {
            let lua = Lua::new();
            let scheduler = Scheduler::new(lua.clone());
            lua.globals().set("console", console).unwrap();
            lua.globals()
                .set(
                    "read",
                    lua.create_async_function(move |lua, ()| async move {
                        let cancellation = terminal::InputCancellation::default();
                        let _input =
                            acquire_input(&cancellation, input_gate(console), &lua, || {}).await?;
                        Ok(INPUT_GATE.try_lock_arc().is_none())
                    })
                    .unwrap(),
                )
                .unwrap();
            scheduler
                .push_thread_front(lua.load("assert(read() == console); verified = true"), ())
                .unwrap();
            futures_lite::future::block_on(scheduler.run());
            assert!(lua.globals().get::<bool>("verified").unwrap());
            assert!(INPUT_GATE.try_lock_arc().is_some());
            assert!(REDIRECTED_INPUT_GATE.try_lock_arc().is_some());
        }
    }
}
