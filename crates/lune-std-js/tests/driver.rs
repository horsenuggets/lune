#![allow(clippy::cargo_common_metadata)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_io::{Timer, block_on};
use mlua::prelude::*;
use mlua_luau_scheduler::{LuaSpawnExt, Scheduler};

fn run(script: &str) -> LuaResult<(Lua, Vec<LuaValue>)> {
    let lua = Lua::new();
    let module = lune_std_js::module(lua.clone())?;
    lua.register_module("@lune/js", module)?;

    let fair_marker = Arc::new(AtomicBool::new(false));
    let start_fair_marker = lua.create_function({
        let fair_marker = fair_marker.clone();
        move |lua, (): ()| {
            let fair_marker = fair_marker.clone();
            lua.spawn(async move {
                fair_marker.store(true, Ordering::SeqCst);
            })
            .detach();
            Ok(())
        }
    })?;
    lua.globals().set("startFairMarker", start_fair_marker)?;
    let read_fair_marker =
        lua.create_function(move |_, (): ()| Ok(fair_marker.load(Ordering::SeqCst)))?;
    lua.globals().set("fairMarker", read_fair_marker)?;

    let sleep = lua.create_async_function(|_, milliseconds: u64| async move {
        Timer::after(Duration::from_millis(milliseconds)).await;
        Ok(())
    })?;
    lua.globals().set("sleep", sleep)?;

    let scheduler = Scheduler::new(lua.clone());
    let thread = lua.load(script).into_function()?;
    let thread_id = scheduler.push_thread_back(thread, ())?;
    block_on(scheduler.run());

    let values = scheduler
        .get_thread_result(thread_id)
        .expect("the script result should be tracked")?
        .into_iter()
        .collect();
    Ok((lua, values))
}

#[test]
fn evaluates_primitives_and_sentinels() -> LuaResult<()> {
    let (lua, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext({
            MemoryLimitBytes = 16 * 1024 * 1024,
            StackLimitBytes = 256 * 1024,
            ExecutionLimitMs = 1_000,
            OperationTimeoutMs = 1_000,
            JobBudget = 8,
        })
        local number = context:Evaluate("6 * 7")
        local string = context:Evaluate("'hello ' + 'world'")
        local boolean = context:Evaluate("1 < 2")
        local null = context:Evaluate("null")
        local undefined = context:Evaluate("undefined")
        assert(null == js.Null)
        assert(undefined == js.Undefined)
        assert(null ~= undefined)
        context:Close()
        return number, string, boolean, tostring(null), tostring(undefined)
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Number(42.0),
            LuaValue::String(lua.create_string("hello world")?),
            LuaValue::Boolean(true),
            LuaValue::String(lua.create_string("js.Null")?),
            LuaValue::String(lua.create_string("js.Undefined")?),
        ]
    );
    Ok(())
}

#[test]
fn awaits_promises_and_reports_rejections() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        local result = context:Evaluate([[
            Promise.resolve(20).then(async value => {
                const doubled = value * 2
                return doubled + 2
            })
        ]])
        local ok, message = pcall(function()
            context:Evaluate("Promise.reject(new Error('expected rejection'))")
        end)
        context:Close()
        return result, ok, string.find(tostring(message), "expected rejection", 1, true) ~= nil
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Number(42.0),
            LuaValue::Boolean(false),
            LuaValue::Boolean(true),
        ]
    );
    Ok(())
}

#[test]
fn keeps_contexts_isolated() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local first = js.newContext()
        local second = js.newContext()
        first:Evaluate("globalThis.answer = 40")
        second:Evaluate("globalThis.answer = 2")
        local firstAnswer = first:Evaluate("answer")
        local secondAnswer = second:Evaluate("answer")
        first:Close()
        second:Close()
        return firstAnswer, secondAnswer
        "#)?;

    assert_eq!(values, vec![LuaValue::Number(40.0), LuaValue::Number(2.0)]);
    Ok(())
}

#[test]
fn close_is_idempotent_and_invalidates_evaluation() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        local func = context:Evaluate("value => value + 1")
        context:Close()
        context:Close()
        local evalOk, evalMessage = pcall(function()
            context:Evaluate("1 + 1")
        end)
        local callOk, callMessage = pcall(function()
            return func(1)
        end)
        return
            evalOk,
            string.find(tostring(evalMessage), "JavaScript context is closed", 1, true) ~= nil,
            callOk,
            string.find(tostring(callMessage), "JavaScript context is closed", 1, true) ~= nil
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Boolean(false),
            LuaValue::Boolean(true),
            LuaValue::Boolean(false),
            LuaValue::Boolean(true),
        ]
    );
    Ok(())
}

#[test]
fn close_interrupts_an_active_operation() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        context:Expose("closeFromJs", function()
            context:Close()
            context:Close()
        end)
        local ok, message = pcall(function()
            context:Evaluate("closeFromJs(); while (true) {}")
        end)
        return ok, string.find(tostring(message), "JavaScript context is closed", 1, true) ~= nil
        "#)?;

    assert_eq!(
        values,
        vec![LuaValue::Boolean(false), LuaValue::Boolean(true)]
    );
    Ok(())
}

#[test]
fn idle_open_context_does_not_keep_scheduler_alive() -> LuaResult<()> {
    let (lua, values) = run(r#"
        local js = require("@lune/js")
        contextForIdleTest = js.newContext()
        return contextForIdleTest:Evaluate("'ready'")
        "#)?;

    assert_eq!(values, vec![LuaValue::String(lua.create_string("ready")?)]);
    assert!(
        lua.globals()
            .get::<LuaAnyUserData>("contextForIdleTest")
            .is_ok()
    );
    Ok(())
}

#[test]
fn converts_nested_javascript_values_and_cycles() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        local value = context:Evaluate([[
            (() => {
                const shared = { answer: 42 }
                const nullPrototype = Object.assign(Object.create(null), { works: true })
                const root = {
                    array: [1, "two", true, null, undefined],
                    shared,
                    alias: shared,
                    nullPrototype,
                }
                root.self = root
                return root
            })()
        ]])
        context:Close()
        return
            value.array[1],
            value.array[2] == "two",
            value.array[3],
            value.array[4] == js.Null,
            value.array[5] == js.Undefined,
            value.shared == value.alias,
            value.self == value,
            value.nullPrototype.works,
            value.shared.answer
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Number(1.0),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Number(42.0),
        ]
    );
    Ok(())
}

#[test]
fn converts_lua_tables_buffers_sentinels_and_cycles() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        context:Expose("makeValue", function()
            local shared = { answer = 42 }
            local bytes = buffer.create(3)
            buffer.writeu8(bytes, 0, 1)
            buffer.writeu8(bytes, 1, 2)
            buffer.writeu8(bytes, 2, 255)
            local root = {
                array = { 1, "two", true, js.Null, js.Undefined },
                bytes = bytes,
                shared = shared,
                alias = shared,
            }
            root.self = root
            return root
        end)
        local value = context:Evaluate([[
            (() => {
                const value = makeValue()
                return [
                    value.array[0],
                    value.array[1],
                    value.array[2],
                    value.array[3] === null,
                    value.array[4] === undefined,
                    value.shared === value.alias,
                    value.self === value,
                    value.bytes instanceof Uint8Array,
                    Array.from(value.bytes).join(","),
                    value.shared.answer,
                ]
            })()
        ]])
        local bytes = context:Evaluate("new Uint8Array([1, 2, 255])")
        local arrayBuffer = context:Evaluate("new ArrayBuffer(2)")
        context:Close()
        return
            value[1],
            value[2] == "two",
            value[3],
            value[4],
            value[5],
            value[6],
            value[7],
            value[8],
            value[9] == "1,2,255",
            value[10],
            buffer.readu8(bytes, 0),
            buffer.readu8(bytes, 1),
            buffer.readu8(bytes, 2),
            buffer.readu8(arrayBuffer, 0),
            buffer.readu8(arrayBuffer, 1)
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Number(1.0),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Boolean(true),
            LuaValue::Number(42.0),
            LuaValue::Integer(1),
            LuaValue::Integer(2),
            LuaValue::Integer(255),
            LuaValue::Integer(0),
            LuaValue::Integer(0),
        ]
    );
    Ok(())
}

#[test]
fn rejects_unsupported_values_and_table_shapes() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        local sources = {
            "1n",
            "Symbol('value')",
            "new Date()",
            "new Proxy({}, {})",
            "({ [Symbol('key')]: true })",
        }
        local rejected = 0
        for _, source in sources do
            local ok = pcall(function()
                context:Evaluate(source)
            end)
            assert(not ok)
            rejected += 1
        end

        context:Expose("sparse", function()
            return { [1] = true, [3] = true }
        end)
        context:Expose("mixed", function()
            return { [1] = true, key = true }
        end)
        context:Expose("metatable", function()
            return setmetatable({}, {})
        end)
        for _, name in { "sparse", "mixed", "metatable" } do
            local ok, message = pcall(function()
                context:Evaluate(`{name}()`)
            end)
            assert(not ok)
            assert(string.find(tostring(message), "not supported", 1, true) ~= nil)
            rejected += 1
        end
        context:Close()
        return rejected
        "#)?;

    assert_eq!(values, vec![LuaValue::Integer(8)]);
    Ok(())
}

#[test]
fn calls_javascript_functions_and_preserves_function_identity() -> LuaResult<()> {
    let (lua, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        local add = context:Evaluate("(a, b) => a + b")
        assert(type(add) == "userdata")
        local sum = add(20, 22)
        local asyncAdd = context:Evaluate([[
            async (a, b) => {
                const sum = await Promise.resolve(a + b)
                return sum
            }
        ]])
        local asyncSum = asyncAdd(20, 22)
        context:Expose("identity", function(value)
            return value
        end)
        context:Expose("getAdd", function()
            return add
        end)
        local identityResult = context:Evaluate("identity(getAdd())(40, 2)")
        local other = js.newContext()
        other:Expose("foreignFunction", function()
            return add
        end)
        assert(not pcall(function()
            other:Evaluate("foreignFunction()")
        end))
        other:Close()
        local description = tostring(add)
        context:Close()
        return sum, asyncSum, identityResult, description
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Number(42.0),
            LuaValue::Number(42.0),
            LuaValue::Number(42.0),
            LuaValue::String(lua.create_string("JavaScript function")?),
        ]
    );
    Ok(())
}

#[test]
fn exposes_sync_callbacks_and_multiple_returns() -> LuaResult<()> {
    let (lua, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        context:Expose("add", function(a, b)
            return a + b
        end)
        context:Expose("pair", function(a, b)
            return a, b
        end)
        context:Expose("nothing", function() end)
        context:Expose("nilValue", function()
            return nil
        end)
        local sum = context:Evaluate("add(20, 22)")
        local pair = context:Evaluate("pair('first', 'second')")
        assert(context:Evaluate("nothing()") == js.Undefined)
        assert(context:Evaluate("nilValue()") == js.Null)
        context:Close()
        return sum, pair[1], pair[2]
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Number(42.0),
            LuaValue::String(lua.create_string("first")?),
            LuaValue::String(lua.create_string("second")?),
        ]
    );
    Ok(())
}

#[test]
fn exposes_yielding_async_callbacks() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        context:ExposeAsync("addLater", function(a, b)
            sleep(1)
            return a + b
        end)
        local result = context:Evaluate("addLater(20, 22)")
        context:Close()
        return result
        "#)?;

    assert_eq!(values, vec![LuaValue::Number(42.0)]);
    Ok(())
}

#[test]
fn yielding_callbacks_finish_before_operation_deadlines() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext({ OperationTimeoutMs = 1_000 })
        context:ExposeAsync("addLater", function(a, b)
            sleep(1)
            return a + b
        end)
        local result = context:Evaluate("addLater(20, 22)")
        context:Close()
        return result
        "#)?;

    assert_eq!(values, vec![LuaValue::Number(42.0)]);
    Ok(())
}

#[test]
fn rejects_reentrant_operations() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        context:Expose("reenter", function()
            local ok, message = pcall(function()
                context:Evaluate("1 + 1")
            end)
            assert(not ok)
            return tostring(message)
        end)
        local message = context:Evaluate("reenter()")
        context:Close()
        return string.find(message, "not reentrant", 1, true) ~= nil
        "#)?;

    assert_eq!(values, vec![LuaValue::Boolean(true)]);
    Ok(())
}

#[test]
fn applies_filename_and_module_evaluation_options() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        local ok, message = pcall(function()
            context:Evaluate("throw new Error('named failure')", {
                Filename = "named-script.js",
                Mode = "script",
            })
        end)
        assert(not ok)
        local hasFilename = string.find(tostring(message), "named-script.js", 1, true) ~= nil
        local moduleResult = context:Evaluate([[
            globalThis.moduleValue = await Promise.resolve(42)
        ]], {
            Filename = "named-module.js",
            Mode = "module",
        })
        assert(moduleResult == js.Undefined)
        local value = context:Evaluate("moduleValue")
        context:Close()
        return hasFilename, value
        "#)?;

    assert_eq!(
        values,
        vec![LuaValue::Boolean(true), LuaValue::Number(42.0)]
    );
    Ok(())
}

#[test]
fn validates_context_and_evaluation_options() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext()
        local cases = {
            function() js.newContext({ MemoryLimitBytes = -1 }) end,
            function() js.newContext({ StackLimitBytes = 1.5 }) end,
            function() js.newContext({ ExecutionLimitMs = 0 }) end,
            function() js.newContext({ OperationTimeoutMs = 0 }) end,
            function() js.newContext({ JobBudget = 0 }) end,
            function() js.newContext({ Unknown = true }) end,
            function() context:Evaluate("1", { Filename = 1 }) end,
            function() context:Evaluate("1", { Mode = "unknown" }) end,
            function() context:Evaluate("1", { Unknown = true }) end,
        }
        local rejected = 0
        for _, case in cases do
            assert(not pcall(case))
            rejected += 1
        end
        context:Close()
        return rejected
        "#)?;

    assert_eq!(values, vec![LuaValue::Integer(9)]);
    Ok(())
}

#[test]
fn enforces_execution_limits() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext({ ExecutionLimitMs = 5 })
        local ok, message = pcall(function()
            context:Evaluate("while (true) {}")
        end)
        local result = context:Evaluate("40 + 2")
        context:Close()
        return
            ok,
            string.find(tostring(message), "JavaScript execution limit exceeded", 1, true) ~= nil,
            result
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Boolean(false),
            LuaValue::Boolean(true),
            LuaValue::Number(42.0),
        ]
    );
    Ok(())
}

#[test]
fn enforces_operation_timeouts_while_idle() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext({ OperationTimeoutMs = 10 })
        local ok, message = pcall(function()
            context:Evaluate("new Promise(() => {})")
        end)
        local result = context:Evaluate("40 + 2")
        context:Close()
        return
            ok,
            string.find(tostring(message), "JavaScript operation timed out", 1, true) ~= nil,
            result
        "#)?;

    assert_eq!(
        values,
        vec![
            LuaValue::Boolean(false),
            LuaValue::Boolean(true),
            LuaValue::Number(42.0),
        ]
    );
    Ok(())
}

#[test]
fn enforces_operation_timeouts_during_async_callbacks() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext({ OperationTimeoutMs = 5 })
        context:ExposeAsync("slowCallback", function()
            sleep(100)
            return true
        end)
        local ok, message = pcall(function()
            context:Evaluate("slowCallback()")
        end)
        context:Close()
        return
            ok,
            string.find(tostring(message), "JavaScript operation timed out", 1, true) ~= nil
        "#)?;

    assert_eq!(
        values,
        vec![LuaValue::Boolean(false), LuaValue::Boolean(true)]
    );
    Ok(())
}

#[test]
fn yields_between_promise_jobs_using_the_job_budget() -> LuaResult<()> {
    let (_, values) = run(r#"
        local js = require("@lune/js")
        local context = js.newContext({ JobBudget = 1 })
        startFairMarker()
        local result = context:Evaluate([[
            new Promise(resolve => {
                let value = 0
                function step() {
                    value += 1
                    if (value === 200) {
                        resolve(value)
                    } else {
                        Promise.resolve().then(step)
                    }
                }
                step()
            })
        ]])
        context:Close()
        return result, fairMarker()
        "#)?;

    assert_eq!(
        values,
        vec![LuaValue::Number(200.0), LuaValue::Boolean(true)]
    );
    Ok(())
}
