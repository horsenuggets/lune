#![allow(clippy::cargo_common_metadata)]
#![cfg(all(
    feature = "cli",
    feature = "std-fs",
    feature = "std-process",
    feature = "std-stdio"
))]

use std::{
    fs,
    io::{ErrorKind, Write},
    process::{Command, Output, Stdio},
};

const ECHO_CHILD: &str = r#"
    const stdio = require("@lune/stdio")
    stdio.write(stdio.readToEnd())
"#;
const INPUT: &[u8] = b"Native stdin\0with binary\xff\r\nand UTF-8 \xc3\xa9\n";
const SENTINEL_CHILD: &str = r#"
    const fs = require("@lune/fs")
    const process = require("@lune/process")
    fs.writeFile(process.args[1], "Child was launched.")
"#;

fn run_parent(source: &str, child_source: &str, input: &[u8]) -> Output {
    let temporary = tempfile::tempdir().unwrap();
    let parent = temporary.path().join("Parent.luau");
    let child = temporary.path().join("Child.luau");
    let sentinel = temporary.path().join("Sentinel.txt");
    fs::write(
        &parent,
        format!(
            r#"
                const process = require("@lune/process")
                const stdio = require("@lune/stdio")
                const args = {{ "run", process.args[2], process.args[3] }}
                {source}
            "#
        ),
    )
    .unwrap();
    fs::write(&child, child_source).unwrap();
    let mut parent = Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&parent)
        .arg(env!("CARGO_BIN_EXE_lune"))
        .arg(&child)
        .arg(&sentinel)
        .current_dir(temporary.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Buffered input and rejected options need not read the parent's native stdin.
    if let Err(error) = parent.stdin.take().unwrap().write_all(input) {
        assert_eq!(error.kind(), ErrorKind::BrokenPipe, "{error}");
    }
    let output = parent.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Parent failed for {source}.\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!sentinel.exists(), "Rejected options launched the child.");
    output
}

#[test]
fn sentinel_fixture_records_child_execution() {
    let temporary = tempfile::tempdir().unwrap();
    let script = temporary.path().join("Child.luau");
    let sentinel = temporary.path().join("Sentinel.txt");
    fs::write(&script, SENTINEL_CHILD).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lune"))
        .arg("run")
        .arg(&script)
        .arg(&sentinel)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Sentinel fixture failed. {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "Child was launched.");
}

#[test]
fn exec_inherits_native_stdin_when_requested() {
    let output = run_parent(
        r"
            const result = process.exec(process.args[1], args, { inheritStdin = true })
            assert(result.ok, result.stderr)
            stdio.write(result.stdout)
        ",
        ECHO_CHILD,
        INPUT,
    );
    assert_eq!(output.stdout, INPUT);
    assert!(output.stderr.is_empty());
}

#[test]
fn exec_default_and_false_stdin_remain_eof() {
    for options in [
        "",
        ", nil",
        ", {}",
        ", { inheritStdin = false }",
        ", { stdio = \"inherit\" }",
        ", { stdio = \"forward\" }",
    ] {
        let output = run_parent(
            &format!(
                r#"
                    const result = process.exec(process.args[1], args{options})
                    assert(result.ok, result.stderr)
                    assert(result.stdout == "", "Child stdin must be EOF.")
                    stdio.write(stdio.readToEnd())
                "#
            ),
            ECHO_CHILD,
            INPUT,
        );
        // The parent must still have all its input, even for inherited/forwarded output.
        assert_eq!(output.stdout, INPUT, "Unexpected output for {options}.");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn exec_preserves_buffered_stdin_and_literal_strings() {
    for (stdin, expected) in [
        (
            r#""buffered\000\255\r\n""#,
            b"buffered\0\xff\r\n".as_slice(),
        ),
        (
            r#"buffer.fromstring("buffered\000\255\r\n")"#,
            b"buffered\0\xff\r\n".as_slice(),
        ),
        (r#""inherit""#, b"inherit".as_slice()),
        (r#""forward""#, b"forward".as_slice()),
        (r#""""#, b"".as_slice()),
        ("buffer.create(0)", b"".as_slice()),
    ] {
        for inherit in ["", "inheritStdin = false,"] {
            let output = run_parent(
                &format!(
                    r"
                        const result = process.exec(process.args[1], args, {{
                            {inherit}
                            stdio = {{ stdin = {stdin} }},
                        }})
                        assert(result.ok, result.stderr)
                        stdio.write(result.stdout)
                    "
                ),
                ECHO_CHILD,
                INPUT,
            );
            assert_eq!(output.stdout, expected, "Unexpected output for {stdin}.");
            assert!(output.stderr.is_empty());
        }
    }
}

#[test]
fn exec_inherited_stdin_preserves_output_modes() {
    for mode in ["inherit", "forward"] {
        let output = run_parent(
            &format!(
                r#"
                    const result = process.exec(process.args[1], args, {{
                        inheritStdin = true,
                        stdio = "{mode}",
                    }})
                    assert(result.ok, result.stderr)
                    stdio.ewrite(result.stdout)
                "#
            ),
            ECHO_CHILD,
            INPUT,
        );
        assert_eq!(output.stdout, INPUT);
        assert_eq!(
            output.stderr,
            if mode == "inherit" { INPUT } else { &[] },
            "Output capture changed for {mode}."
        );
    }
}

#[test]
fn exec_rejects_conflicting_stdin_before_spawning() {
    for stdin in [
        r#""bytes""#,
        r#""""#,
        "buffer.fromstring(\"bytes\")",
        "buffer.create(0)",
    ] {
        run_parent(
            &format!(
                r#"
                    const ok, message = pcall(process.exec, process.args[1], args, {{
                        inheritStdin = true,
                        stdio = {{ stdin = {stdin} }},
                    }})
                    assert(not ok, "Conflicting stdin must be rejected.")
                    assert(string.find(tostring(message),
                        "inheritStdin cannot be combined with stdio.stdin", 1, true), message)
                "#
            ),
            SENTINEL_CHILD,
            INPUT,
        );
    }
}

#[test]
fn invalid_inherit_stdin_is_rejected_before_spawning() {
    for method in ["exec", "create"] {
        for value in ["0", "\"true\"", "{}"] {
            run_parent(
                &format!(
                    r#"
                        const ok, message = pcall(process.{method}, process.args[1], args, {{
                            inheritStdin = {value},
                        }})
                        assert(not ok, "Non-boolean inheritStdin must be rejected.")
                        assert(string.find(tostring(message),
                            "Invalid type for option 'inheritStdin'", 1, true), message)
                    "#
                ),
                SENTINEL_CHILD,
                INPUT,
            );
        }
    }
}

#[test]
fn create_rejects_inherited_stdin_before_spawning() {
    for owned in ["", "ownedTree = true,"] {
        run_parent(
            &format!(
                r#"
                    const ok, message = pcall(process.create, process.args[1], args, {{
                        {owned}
                        inheritStdin = true,
                    }})
                    assert(not ok, "process.create must reject inheritStdin.")
                    assert(string.find(tostring(message),
                        "inheritStdin is only supported by process.exec", 1, true), message)
                "#
            ),
            SENTINEL_CHILD,
            INPUT,
        );
    }
}
