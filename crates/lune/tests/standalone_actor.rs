#![allow(clippy::cargo_common_metadata)]
#![cfg(all(feature = "cli", feature = "std-actor"))]

use std::{fs, process::Command};

#[test]
fn standalone_actors_match_normal_execution_after_sources_are_hidden() {
    let temporary = tempfile::tempdir().unwrap();
    let sources = temporary.path().join("Sources");
    let workers = sources.join("Workers");
    let nested = sources.join("Nested");
    fs::create_dir_all(&workers).unwrap();
    fs::create_dir_all(&nested).unwrap();
    let main = sources.join("Main.luau");
    fs::write(
        &main,
        r#"
            const actor = require("@lune/actor")
            const workerPath = "./" .. "Workers"
            const ok, result = actor.spawn(workerPath):join()
            assert(ok, result)
            print(result)
        "#,
    )
    .unwrap();
    fs::write(
        workers.join("init.luau"),
        r#"#!/usr/bin/env lune
            const actor = require("@lune/actor")
            const prefix = require("./Dependency")
            const ok, result = actor.spawn("../Nested/Child.luau"):join()
            assert(ok, result)
            return prefix .. result
        "#,
    )
    .unwrap();
    fs::write(workers.join("Dependency.luau"), "return 'worker-'").unwrap();
    fs::write(nested.join("Child.luau"), "return 'nested'").unwrap();
    let normal = Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&sources)
        .output()
        .unwrap();
    assert!(
        normal.status.success(),
        "{}",
        String::from_utf8_lossy(&normal.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&normal.stdout), "worker-nested\n");
    let binary = temporary.path().join(if cfg!(windows) {
        "Actors.exe"
    } else {
        "Actors"
    });
    let build = Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .arg("--include")
        .arg("Workers/init.luau")
        .arg("--include")
        .arg("Nested/*.luau")
        .current_dir(&sources)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    fs::rename(&sources, temporary.path().join("Hidden")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = Command::new(&binary)
        .current_dir(empty.path())
        .output()
        .unwrap();
    assert!(
        standalone.status.success(),
        "{}",
        String::from_utf8_lossy(&standalone.stderr)
    );
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_actors_reject_unbundled_workers_even_when_host_files_exist() {
    let temporary = tempfile::tempdir().unwrap();
    let main = temporary.path().join("Main.luau");
    let worker = temporary.path().join("Worker.luau");
    fs::write(&worker, "return 'host'").unwrap();
    fs::write(
        &main,
        r#"
            const actor = require("@lune/actor")
            const process = require("@lune/process")
            const ok, result = actor.spawn(process.args[1]):join()
            assert(not ok, "A missing bundled worker must not run a host file.")
            assert(string.find(result, "not embedded", 1, true), result)
            print("Missing worker rejected.")
        "#,
    )
    .unwrap();
    let binary = temporary.path().join(if cfg!(windows) {
        "Missing.exe"
    } else {
        "Missing"
    });
    let build = Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(temporary.path())
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let empty = tempfile::tempdir().unwrap();
    let standalone = Command::new(&binary)
        .arg(&worker)
        .current_dir(empty.path())
        .output()
        .unwrap();
    assert!(
        standalone.status.success(),
        "{}",
        String::from_utf8_lossy(&standalone.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&standalone.stdout),
        "Missing worker rejected.\n"
    );
}
