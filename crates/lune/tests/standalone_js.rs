#![cfg(all(feature = "cli", feature = "std-js"))]
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

#[test]
fn standalone_include_only_root_still_uses_namespace() {
    let (_temporary, root) = fixture_root_from("standalone-js-include-only", "IncludeOnly");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "IncludeOnly.exe"
    } else {
        "IncludeOnly"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal include-only execution");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .arg("--js-include")
        .arg("Source/Value.ts")
        .current_dir(&root));
    assert_success(&build, "include-only standalone build");

    hide_sources(&root);
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "include-only standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_anchors_roots_at_the_entry_directory() {
    let (_temporary, root) = fixture_root_from("standalone-js-anchored", "Anchored");
    let main = root.join("app").join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "Anchored.exe"
    } else {
        "Anchored"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal anchored execution");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "anchored standalone build");

    fs::rename(root.join("app"), root.join("appHidden")).unwrap();
    fs::rename(root.join("Shared"), root.join("SharedHidden")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "anchored standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_rooted_subdirectory_worker_matches_normal_execution() {
    let (_temporary, root) = fixture_root_from("standalone-js-rooted-subdir", "RootedSub");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "RootedSub.exe"
    } else {
        "RootedSub"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal rooted subdirectory execution");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "rooted subdirectory standalone build");

    fs::rename(root.join("main.luau"), root.join("main-hidden.luau")).unwrap();
    fs::rename(root.join("Source"), root.join("SourceHidden")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "rooted subdirectory standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_relocates_roots_above_the_entry_directory() {
    let (_temporary, root) = fixture_root_from("standalone-js-parent-root", "ParentRoot");
    let main = root.join("app").join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "ParentRoot.exe"
    } else {
        "ParentRoot"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal parent-root execution");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "parent-root standalone build");

    fs::rename(root.join("app"), root.join("appHidden")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "parent-root standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_relocates_absolute_module_roots() {
    let (_temporary, root) = fixture_root_from("standalone-js-absolute-root", "AbsoluteRoot");
    let main = root.join("main.luau");
    let root_literal = root.to_string_lossy().replace('\\', "/");
    fs::write(
        &main,
        format!(
            "const js = require(\"@lune/js\")\n\nconst Context = js.newContext({{ ModuleRoot = \"{root_literal}\" }})\nconst Value = Context:Import(\"./Value.ts\")\nprint(Value.Which)\nContext:Close()\n"
        ),
    )
    .unwrap();
    let binary = root.join(if cfg!(windows) {
        "AbsoluteRoot.exe"
    } else {
        "AbsoluteRoot"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal absolute-root execution");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "absolute-root standalone build");

    fs::rename(&main, root.join("main-hidden.luau")).unwrap();
    fs::rename(root.join("Value.ts"), root.join("ValueHidden.ts")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "absolute-root standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn standalone_anchors_module_root_to_an_init_entry() {
    let (_temporary, root) = fixture_root_from("standalone-js-init", "Init");
    let main = root.join("app").join("init.luau");
    let binary = root.join(if cfg!(windows) { "Init.exe" } else { "Init" });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal init execution");
    assert_eq!(String::from_utf8_lossy(&normal.stdout), "42\n");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert_success(&build, "init standalone build");

    fs::rename(root.join("app"), root.join("appHidden")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "init standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[cfg(unix)]
#[test]
fn standalone_embeds_included_modules_under_their_alias() {
    let (_temporary, root) = fixture_root_from("standalone-js-symlink", "Alias");
    let main = root.join("main.luau");
    std::os::unix::fs::symlink("Real.ts", root.join("Source").join("Alias.ts")).unwrap();
    let binary = root.join("Alias");

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal alias execution");
    assert_eq!(String::from_utf8_lossy(&normal.stdout), "42\n");

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .arg("--js-include")
        .arg("Source/Alias.ts")
        .current_dir(&root));
    assert_success(&build, "alias standalone build");

    hide_sources(&root);
    fs::remove_file(root.join("SourceHidden").join("Alias.ts")).unwrap();
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "alias standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}

#[test]
fn js_include_embeds_modules_for_opaque_context_options() {
    let (_temporary, root) = fixture_root_from("standalone-js-opaque", "Opaque");
    let main = root.join("main.luau");
    let binary = root.join(if cfg!(windows) {
        "Opaque.exe"
    } else {
        "Opaque"
    });

    let normal = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&main)
        .current_dir(&root));
    assert_success(&normal, "normal opaque execution");
    assert_eq!(String::from_utf8_lossy(&normal.stdout), "42\n");

    let without_include = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .current_dir(&root));
    assert!(!without_include.status.success());
    let errors = String::from_utf8_lossy(&without_include.stderr);
    assert!(
        errors.contains("options the build cannot inspect"),
        "unexpected error: {errors}"
    );

    let build = run(Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("build")
        .arg(&main)
        .arg("--output")
        .arg(&binary)
        .arg("--js-include")
        .arg("Source/Value.ts")
        .current_dir(&root));
    assert_success(&build, "opaque standalone build with includes");

    hide_sources(&root);
    let empty = tempfile::tempdir().unwrap();
    let standalone = run(Command::new(&binary).current_dir(empty.path()));
    assert_success(&standalone, "opaque standalone execution");
    assert_eq!(standalone.stdout, normal.stdout);
}
