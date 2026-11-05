#![allow(clippy::cargo_common_metadata)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn copy_directory(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fixture_root(name: &str) -> (tempfile::TempDir, PathBuf) {
    fixture_root_from("standalone-js", name)
}

fn fixture_root_from(fixture_name: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join(name);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(fixture_name);
    fs::create_dir_all(&root).unwrap();
    copy_directory(&fixture, &root);
    (temporary, root)
}

fn run(command: &mut Command) -> Output {
    command.output().unwrap_or_else(|error| {
        panic!("failed to execute {:?}. {error}", command);
    })
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed with {}\nstdout\n{}\nstderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn hide_sources(root: &Path) {
    fs::rename(root.join("main.luau"), root.join("main-hidden.luau")).unwrap();
    fs::rename(root.join("Source"), root.join("SourceHidden")).unwrap();
}

#[test]
fn standalone_javascript_matches_normal_execution_without_source_files() {
    let (_temporary, root) = fixture_root("Normal");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "Figterm.exe"
    } else {
        "Figterm"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal execution");
    assert_eq!(
        String::from_utf8_lossy(&normal.stdout),
        "ProjectList.Default → XSH, SSH CFG [80x24]\nProjectList.Empty → No projects [40x12]\n"
    );

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .arg("--js-include")
        .arg("Source/Extra.ts")
        .current_dir(&root));
    assert_success(&build, "standalone build");

    hide_sources(&root);
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "standalone execution without sources");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn missing_computed_import_never_falls_back_to_host_files() {
    let (_temporary, root) = fixture_root("Missing");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "Missing.exe"
    } else {
        "Missing"
    });

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "standalone build without dynamic inclusion");

    hide_sources(&root);
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert!(!standalone.status.success());
    let errors = format!(
        "{}{}",
        String::from_utf8_lossy(&standalone.stdout),
        String::from_utf8_lossy(&standalone.stderr)
    );
    assert!(
        errors.contains("not embedded"),
        "unexpected standalone error\n{errors}"
    );
}

#[test]
fn standalone_honors_module_root_without_source_files() {
    let (_temporary, root) = fixture_root_from("standalone-js-rooted", "Rooted");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "Rooted.exe"
    } else {
        "Rooted"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal rooted execution");
    assert_eq!(
        String::from_utf8_lossy(&normal.stdout),
        "Rooted.Default → XSH [80x24]\n"
    );

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .arg("--js-include")
        .arg("Source/Extra.ts")
        .current_dir(&root));
    assert_success(&build, "rooted standalone build");

    hide_sources(&root);
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "rooted standalone execution without sources");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_supports_first_import_from_a_subdirectory() {
    let (_temporary, root) = fixture_root_from("standalone-js-subdir", "Subdir");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "Subdir.exe"
    } else {
        "Subdir"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal subdirectory execution");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "subdirectory standalone build");

    fs::rename(root.join("main.luau"), root.join("main-hidden.luau")).unwrap();
    fs::rename(root.join("Sub"), root.join("SubHidden")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "subdirectory standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_keeps_multiple_module_roots_separate() {
    let (_temporary, root) = fixture_root_from("standalone-js-multiroot", "Multiroot");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "Multiroot.exe"
    } else {
        "Multiroot"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal multi-root execution");
    assert_eq!(String::from_utf8_lossy(&normal.stdout), "A\nB\n");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "multi-root standalone build");

    fs::rename(root.join("main.luau"), root.join("main-hidden.luau")).unwrap();
    fs::rename(root.join("A"), root.join("AHidden")).unwrap();
    fs::rename(root.join("B"), root.join("BHidden")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "multi-root standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}
