//! Native, self-time-limited fixtures shared by the owned-process Luau regressions.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use mlua::prelude::*;

use crate::Runtime;

const FIXTURE: &str = "tests::owned_process::process_owned_fixture";
const DIRECTORY_ENV: &str = "LUNE_PROCESS_OWNED_DIRECTORY";
const ROLE_ENV: &str = "LUNE_PROCESS_OWNED_ROLE";

#[test]
fn process_owned_fixture() -> Result<()> {
    let Some(role) = std::env::var_os(ROLE_ENV) else {
        return Ok(());
    };
    let directory = PathBuf::from(std::env::var_os(DIRECTORY_ENV).unwrap());
    // Even a blocked native write or a failing test cannot leave a fixture alive.
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(30));
        std::process::exit(78);
    });
    let role = role.to_str().unwrap();
    if role == "owner-pool" {
        async_io::block_on(async {
            let result = runtime(&directory)?
                .run_custom(
                    "owned-small-blocking-pool",
                    include_str!("../../../tests/process/create/owned_tree.luau"),
                )
                .await?;
            anyhow::ensure!(result.success(), "{}", result.errors.join("\n"));
            Ok::<_, anyhow::Error>(())
        })?;
        return Ok(());
    }
    if role == "owner-abrupt" {
        async_io::block_on(async {
            let mut runtime = runtime(&directory)?.with_lib("@test/native-exit", |lua| {
                let exit =
                    lua.create_function(|_, ()| -> LuaResult<()> { std::process::exit(0) })?;
                lua.globals()
                    .get::<LuaTable>("_G")?
                    .set("NativeExit", exit.clone())?;
                Ok(LuaValue::Function(exit))
            })?;
            runtime
                .run_custom(
                    "abrupt-owner",
                    r#"
                const process = require("@lune/process")
                const task = require("@lune/task")
                const fixture = _G.OwnedProcessFixture
                _G.AbandonedOwnedProcess = process.create(fixture.Executable, fixture.Args, {
                    env = fixture.Environment("launcher"), ownedTree = true,
                })
                const deadline = os.clock() + 5
                while fixture.Read("grandchild-heartbeat") == nil do
                    assert(os.clock() < deadline)
                    task.wait(0.01)
                end
                _G.NativeExit()
            "#,
                )
                .await?;
            Ok::<_, anyhow::Error>(())
        })?;
        panic!("native exit returned");
    }
    if role == "configuration" {
        assert_eq!(std::env::var("LUNE_PROCESS_OWNED_TOKEN")?, "configured");
        assert_eq!(
            fs::canonicalize(std::env::current_dir()?)?,
            fs::canonicalize(directory)?
        );
        return Ok(());
    }
    if role == "stdin" {
        let expected = b"owned stdin payload\0\n".repeat(8192);
        let mut received = vec![0_u8; expected.len()];
        io::stdin().read_exact(&mut received)?;
        assert_eq!(received, expected);
        io::stdout().write_all(b"Owned stdin payload received.\n")?;
        io::stdout().flush()?;
        let mut trailing = Vec::new();
        io::stdin().read_to_end(&mut trailing)?;
        assert!(trailing.is_empty());
        io::stdout().write_all(&received)?;
        io::stderr().write_all(b"Owned stdin EOF received.")?;
        return Ok(());
    }
    if role == "launcher" || role == "early" {
        Command::new(std::env::current_exe()?)
            .args(["--exact", FIXTURE, "--nocapture"])
            .env(ROLE_ENV, "grandchild")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        fs::write(directory.join(format!("{role}-ready")), [])?;
        if role == "early" {
            return Ok(());
        }
    } else {
        fs::write(directory.join(format!("{role}-ready")), [])?;
    }
    let start = Instant::now();
    let mut iteration = 0_u64;
    while start.elapsed() < Duration::from_secs(25) {
        if role == "grandchild" || role == "unrelated" {
            fs::write(
                directory.join(format!("{role}-heartbeat")),
                iteration.to_string(),
            )?;
            iteration += 1;
        } else if role == "output" {
            io::stdout().write_all(&[b'O'; 4096])?;
            io::stderr().write_all(&[b'E'; 4096])?;
            continue;
        }
        // In particular, blocked-stdin never reads its input.
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn runtime(directory: &Path) -> Result<Runtime> {
    let directory = directory.to_path_buf();
    let executable = std::env::current_exe()?;
    Ok(
        Runtime::new()?.with_lib("@test/owned-process-fixture", move |lua| {
            let fixture = lua.create_table()?;
            fixture.set("Collect", lua.create_function(|lua, ()| lua.gc_collect())?)?;
            fixture.set("Executable", executable.to_string_lossy().into_owned())?;
            fixture.set("Directory", directory.to_string_lossy().into_owned())?;
            fixture.set(
                "Args",
                lua.create_sequence_from(["--exact", FIXTURE, "--nocapture"])?,
            )?;
            let environment_directory = directory.clone();
            fixture.set(
                "Environment",
                lua.create_function(move |lua, role: String| {
                    lua.create_table_from([
                        (
                            DIRECTORY_ENV,
                            environment_directory.to_string_lossy().into_owned(),
                        ),
                        (ROLE_ENV, role),
                        ("LUNE_PROCESS_OWNED_TOKEN", "configured".into()),
                    ])
                })?,
            )?;
            fixture.set(
                "Read",
                lua.create_function(move |_, name: String| {
                    match fs::read_to_string(directory.join(name)) {
                        Ok(value) => Ok(Some(value)),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                        Err(error) => Err(LuaError::external(error)),
                    }
                })?,
            )?;
            lua.globals()
                .get::<LuaTable>("_G")?
                .set("OwnedProcessFixture", fixture.clone())?;
            Ok(LuaValue::Table(fixture))
        })?,
    )
}

#[cfg(feature = "std-task")]
#[test]
fn process_owned_tree() -> Result<()> {
    let directory = tempfile::tempdir()?;
    async_io::block_on(async {
        let result = runtime(directory.path())?
            .run_custom(
                "process-owned-tree",
                include_str!("../../../tests/process/create/owned_tree.luau"),
            )
            .await?;
        anyhow::ensure!(result.success(), "{}", result.errors.join("\n"));
        Ok(())
    })
}

#[cfg(feature = "std-task")]
#[test]
fn process_owned_owner_shutdown() -> Result<()> {
    for ending in ["return", "exit", "error"] {
        let directory = tempfile::tempdir()?;
        async_io::block_on(async {
            let mut runtime = runtime(directory.path())?;
            let source = format!(
                r#"
                const process = require("@lune/process")
                const task = require("@lune/task")
                const fixture = _G.OwnedProcessFixture
                _G.AbandonedOwnedProcess = process.create(fixture.Executable, fixture.Args, {{
                    env = fixture.Environment("early"), ownedTree = true,
                }})
                const deadline = os.clock() + 5
                while fixture.Read("grandchild-heartbeat") == nil do
                    assert(os.clock() < deadline)
                    task.wait(0.01)
                end
                {}
            "#,
                match ending {
                    "return" => "return true",
                    "exit" => "process.exit(0)",
                    _ => "error('controlled owner error')",
                }
            );
            let result = runtime.run_custom("owned-owner-shutdown", source).await?;
            assert_eq!(result.success(), ending != "error");
            // The global deliberately retains the handle, ruling out incidental GC.
            assert_heartbeat_stopped(directory.path(), "grandchild")?;
            let result = runtime
                .run_custom(
                    "owned-after-shutdown",
                    r"
                _G.AbandonedOwnedProcess:terminateTree()
                assert(_G.AbandonedOwnedProcess:status().ok)
            ",
                )
                .await?;
            assert!(result.success());
            Ok::<_, anyhow::Error>(())
        })?;
    }
    Ok(())
}

#[test]
fn process_owned_io_with_small_blocking_pool() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", FIXTURE, "--nocapture"])
        .env(DIRECTORY_ENV, directory.path())
        .env(ROLE_ENV, "owner-pool")
        .env("BLOCKING_MAX_THREADS", "2")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()?;
    assert!(
        status.success(),
        "owned pipe I/O exhausted the shared blocking pool"
    );
    Ok(())
}

#[test]
fn process_owned_abrupt_owner_death() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", FIXTURE, "--nocapture"])
        .env(DIRECTORY_ENV, directory.path())
        .env(ROLE_ENV, "owner-abrupt")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()?;
    assert!(status.success());
    assert_heartbeat_stopped(directory.path(), "grandchild")
}

fn assert_heartbeat_stopped(directory: &Path, role: &str) -> Result<()> {
    // Allow an in-flight filesystem write to settle, then compare across many ticks.
    thread::sleep(Duration::from_millis(50));
    let path = directory.join(format!("{role}-heartbeat"));
    let before = fs::read(&path)?;
    thread::sleep(Duration::from_millis(150));
    assert_eq!(before, fs::read(path)?, "a descendant survived cleanup");
    Ok(())
}

#[cfg(feature = "std-task")]
#[test]
fn process_owned_canceled_run() -> Result<()> {
    let directory = tempfile::tempdir()?;
    async_io::block_on(async {
        let mut runtime = runtime(directory.path())?;
        let run = runtime.run_custom(
            "canceled-owner",
            r#"
            const process = require("@lune/process")
            const task = require("@lune/task")
            const fixture = _G.OwnedProcessFixture
            _G.AbandonedOwnedProcess = process.create(fixture.Executable, fixture.Args, {
                env = fixture.Environment("early"), ownedTree = true,
            })
            task.spawn(function() _G.AbandonedOwnedProcess.stdout:readToEnd() end)
            task.spawn(function() _G.AbandonedOwnedProcess.stderr:readToEnd() end)
            task.spawn(function() _G.AbandonedOwnedProcess:status() end)
            task.wait(10)
        "#,
        );
        // Cancel only after the grandchild is running, while retaining the runtime.
        let ready = async {
            let start = Instant::now();
            while !directory.path().join("grandchild-heartbeat").exists() {
                assert!(start.elapsed() < Duration::from_secs(5));
                async_io::Timer::after(Duration::from_millis(10)).await;
            }
        };
        futures_lite::future::race(
            async {
                let _ = run.await;
                panic!("run finished early")
            },
            ready,
        )
        .await;
        assert_heartbeat_stopped(directory.path(), "grandchild")?;
        drop(runtime);
        Ok(())
    })
}
