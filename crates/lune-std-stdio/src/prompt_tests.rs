use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Read, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    pty::{Winsize, openpty},
    sys::termios::{self, LocalFlags},
};

use super::*;

const SCENARIOS: [&str; 9] = [
    "arrow-split",
    "confirm",
    "multiselect",
    "partial",
    "password",
    "repeated",
    "select",
    "text",
    "unicode-split",
];

fn options(kind: PromptKind) -> PromptOptions {
    let colors = match std::env::var("LUNE_PROMPT_COLOR_CASE").as_deref() {
        Ok("rgb" | "plain") => {
            let lua = Lua::new();
            let table = lua.create_table().unwrap();
            table
                .set(
                    "Color",
                    std::env::var("LUNE_PROMPT_COLOR_CASE").unwrap() == "rgb",
                )
                .unwrap();
            for field in ["Accent", "Error", "Muted", "Success", "Text", "Warning"] {
                table.set(field, "#123456").unwrap();
            }
            Some(PromptColors::from_lua(LuaValue::Table(table), &lua).unwrap())
        }
        _ => None,
    };
    PromptOptions {
        kind,
        text: Some("Typeahead".into()),
        default_string: None,
        default_bool: None,
        options: Some(vec!["First".into(), "Second".into()]),
        colors,
    }
}

fn scenario(name: &str) -> (PromptKind, &'static [u8], &'static [u8], &'static str) {
    match name {
        "text" => (
            PromptKind::Text,
            "héllo\rnext".as_bytes(),
            b"",
            "String(\"héllo\")",
        ),
        "password" => (
            PromptKind::Password,
            "secrét\x7fX\rnext".as_bytes(),
            b"",
            "String(\"secréX\")",
        ),
        "confirm" => (PromptKind::Confirm, b"ynext", b"", "Boolean(true)"),
        "select" => (PromptKind::Select, b"\x1b[B\x1b[B\rnext", b"", "Index(2)"),
        "multiselect" => (
            PromptKind::MultiSelect,
            b" \x1b[B \rnext",
            b"",
            "Indices([1, 2])",
        ),
        "partial" => (PromptKind::Text, b"par", b"tial\r", "String(\"partial\")"),
        "unicode-split" => (PromptKind::Text, b"\xc3", b"\xa9\r", "String(\"é\")"),
        "arrow-split" => (PromptKind::Select, b"\x1b[B\x1b[", b"B\r", "Index(2)"),
        "repeated" => (
            PromptKind::Text,
            b"first\rsecond\rnext",
            b"",
            "String(\"first\")",
        ),
        _ => panic!("Unknown prompt fixture."),
    }
}

#[test]
fn prompt_typeahead_worker() {
    let Ok(name) = std::env::var("LUNE_PROMPT_TYPEAHEAD_CASE") else {
        return;
    };
    let (kind, bytes, os_input, expected) = scenario(&name);
    let pending = Arc::new(Mutex::new(VecDeque::from(bytes.to_vec())));
    let terminal = console::Term::stderr().with_pending_input(Arc::clone(&pending));
    let result = prompt_on(options(kind), &terminal).unwrap();
    assert_eq!(format!("{result:?}"), expected);
    if name == "repeated" {
        let result = prompt_on(options(PromptKind::Text), &terminal).unwrap();
        assert_eq!(format!("{result:?}"), "String(\"second\")");
    }
    let remaining = pending.lock().unwrap().iter().copied().collect::<Vec<_>>();
    assert_eq!(
        remaining,
        if os_input.is_empty() {
            b"next".as_slice()
        } else {
            b""
        }
    );
    println!("Prompt typeahead completed.");
}

#[test]
fn redirected_stdin_prompt_worker() {
    use mlua_luau_scheduler::{Functions, Scheduler};

    let Ok(case) = std::env::var("LUNE_REDIRECTED_PROMPT_CHILD") else {
        return;
    };

    let lua = Lua::new();
    let scheduler = Scheduler::new(lua.clone());
    let functions = Functions::new(lua.clone()).unwrap();
    lua.globals().set("spawn", functions.spawn).unwrap();
    lua.globals().set("cancel", functions.cancel).unwrap();
    lua.globals()
        .set("concurrent", case == "concurrent")
        .unwrap();
    lua.globals()
        .set("stdio", crate::module(lua.clone()).unwrap())
        .unwrap();
    scheduler
        .push_thread_front(
            lua.load(
                r#"
assert(stdio.readLine() == "header\n")
local owner
if concurrent then
    owner = spawn(function()
        stdio.readToEnd()
    end)
end
assert(not stdio.prompt("confirm", "Proceed?", false))
if owner then
    cancel(owner)
end
assert(stdio.readLine() == "y\n")
done = true
"#,
            ),
            (),
        )
        .unwrap();
    futures_lite::future::block_on(scheduler.run());
    assert!(lua.globals().get::<bool>("done").unwrap());
    println!("Redirected prompt completed.");
    // Keep the controlling session alive until the parent observes completion.
    assert_eq!(
        crate::terminal::prompt_terminal().read_key().unwrap(),
        console::Key::Char('x')
    );
}

#[test]
fn redirected_stdin_cannot_supply_interactive_prompt_answers() {
    run_redirected_prompt(false);
}

#[test]
fn an_active_pipe_read_does_not_block_controlling_terminal_prompts() {
    run_redirected_prompt(true);
}

fn run_redirected_prompt(concurrent: bool) {
    use std::os::unix::process::CommandExt;

    let pty = openpty(None, None).unwrap();
    let mut master = File::from(pty.master);
    let slave = File::from(pty.slave);
    fcntl(&master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "prompt::tests::redirected_stdin_prompt_worker",
            "--nocapture",
            "--color",
            "never",
        ])
        .env(
            "LUNE_REDIRECTED_PROMPT_CHILD",
            if concurrent {
                "concurrent"
            } else {
                "sequential"
            },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    // Stdin deliberately remains a pipe. Console must use the controlling terminal.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()?;
            if nix::libc::ioctl(2, nix::libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ChildGuard(command.spawn().unwrap());
    drop(command);
    child
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"header\ny\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let mut answered = false;
    let mut acknowledged = false;
    let status = loop {
        let mut buffer = [0; 4096];
        match master.read(&mut buffer) {
            Ok(count) => output.extend_from_slice(&buffer[..count]),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.raw_os_error() == Some(nix::libc::EIO) => {}
            Err(error) => panic!("Reading redirected prompt failed. {error}"),
        }
        let text = String::from_utf8_lossy(&output);
        if !answered && text.contains("Proceed?") {
            master.write_all(b"n").unwrap();
            answered = true;
        }
        if !acknowledged && text.contains("Redirected prompt completed.") {
            master.write_all(b"x").unwrap();
            acknowledged = true;
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "The redirected prompt timed out."
        );
        thread::sleep(Duration::from_millis(2));
    };
    let output = String::from_utf8_lossy(&output);
    assert!(answered && acknowledged && status.success(), "{output}");
    assert!(output.contains("Redirected prompt completed."), "{output}");
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn assert_prompt_colors(output: &str, name: &str, colors: &str) {
    let sgr = output
        .split("\x1b[")
        .skip(1)
        .filter_map(|escape| {
            let end = escape.find('m')?;
            let parameters = &escape[..end];
            parameters
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b';')
                .then_some(parameters)
        })
        .collect::<Vec<_>>();
    if colors == "rgb" {
        assert!(sgr.contains(&"38;2;18;52;86"), "{name} {colors} {output}");
        assert!(
            sgr.iter()
                .all(|parameters| matches!(*parameters, "0" | "1" | "38;2;18;52;86")),
            "{name} {colors} {sgr:?}"
        );
    } else if colors == "plain" {
        assert!(sgr.is_empty(), "{name} {colors} {sgr:?}");
    }
}

#[test]
fn every_prompt_preserves_typeahead_style_and_terminal_mode() {
    for (name, colors) in SCENARIOS.into_iter().flat_map(|name| {
        ["native", "plain", "rgb"]
            .into_iter()
            .map(move |colors| (name, colors))
    }) {
        let pty = openpty(
            Some(&Winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let mut master = File::from(pty.master);
        let slave = File::from(pty.slave);
        let saved = termios::tcgetattr(&slave).unwrap();
        fcntl(&master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "prompt::tests::prompt_typeahead_worker",
                "--nocapture",
                "--color",
                "never",
            ])
            .env("LUNE_PROMPT_TYPEAHEAD_CASE", name)
            .env("LUNE_PROMPT_COLOR_CASE", colors)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        let mut child = ChildGuard(command.spawn().unwrap());
        drop(command);
        master.write_all(scenario(name).2).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let status = loop {
            let mut buffer = [0; 4096];
            match master.read(&mut buffer) {
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("Reading prompt fixture failed. {error}"),
            }
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "Prompt fixture \"{name}\" timed out. {}",
                String::from_utf8_lossy(&output)
            );
            thread::sleep(Duration::from_millis(2));
        };
        let mut buffer = [0; 4096];
        while let Ok(count) = master.read(&mut buffer) {
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
        }
        let output = String::from_utf8_lossy(&output);
        assert!(
            status.success(),
            "Prompt fixture \"{name}\" failed. {output}"
        );
        assert!(
            output.contains("Prompt typeahead completed."),
            "{name} {output}"
        );
        assert_prompt_colors(&output, name, colors);
        if name == "password" {
            assert!(
                !output.contains("secré"),
                "Password input must not be echoed."
            );
        }
        let restored = termios::tcgetattr(&slave).unwrap();
        assert_eq!(restored.input_flags, saved.input_flags, "{name}");
        assert_eq!(restored.output_flags, saved.output_flags, "{name}");
        assert_eq!(restored.control_flags, saved.control_flags, "{name}");
        assert_eq!(
            restored.local_flags & !LocalFlags::PENDIN,
            saved.local_flags & !LocalFlags::PENDIN,
            "{name}"
        );
        assert_eq!(restored.control_chars, saved.control_chars, "{name}");
    }
}
