use async_process::Child;
use futures_lite::prelude::*;
use futures_util::try_join;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use mlua::prelude::*;

use lune_utils::TableBuilder;

use super::options::ProcessSpawnOptionsStdioKind;

mod tee_writer;
mod wait_for_child;

use self::wait_for_child::wait_for_child;

pub async fn exec(
    lua: Lua,
    mut child: Child,
    stdin: Option<Vec<u8>>,
    stdout: ProcessSpawnOptionsStdioKind,
    stderr: ProcessSpawnOptionsStdioKind,
) -> LuaResult<LuaTable> {
    let child_stdin = child.stdin.take();
    let write_stdin = async move {
        if let Some(stdin) = stdin {
            let mut child_stdin =
                child_stdin.expect("Child stdin must be piped when input is supplied");
            child_stdin.write_all(&stdin).await.into_lua_err()?;
            child_stdin.close().await.into_lua_err()?;
        }
        Ok::<(), LuaError>(())
    };
    let ((), res) = try_join!(write_stdin, wait_for_child(child, stdout, stderr))?;

    let code = res.status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        if let Some(signal) = res.status.signal() {
            return 128 + signal;
        }
        1
    });

    // Construct and return a readonly lua table with results
    let stdout = lua.create_string(&res.stdout)?;
    let stderr = lua.create_string(&res.stderr)?;
    TableBuilder::new(lua)?
        .with_value("ok", res.status.success())?
        .with_value("code", code)?
        .with_value("stdout", stdout)?
        .with_value("stderr", stderr)?
        .build_readonly()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn shell_result(
        script: &str,
        stdout: ProcessSpawnOptionsStdioKind,
        stderr: ProcessSpawnOptionsStdioKind,
    ) -> (bool, i32) {
        let child = async_process::Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(stdout.as_stdio())
            .stderr(stderr.as_stdio())
            .spawn()
            .unwrap();
        let lua = Lua::new();
        let result =
            futures_lite::future::block_on(exec(lua.clone(), child, None, stdout, stderr)).unwrap();
        (result.get("ok").unwrap(), result.get("code").unwrap())
    }

    #[test]
    fn signal_failure_does_not_depend_on_output_or_stdio_mode() {
        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
            for output in ["", "printf 'Child diagnostic.\\n' >&2;"] {
                for &stdout in ProcessSpawnOptionsStdioKind::all() {
                    for &stderr in ProcessSpawnOptionsStdioKind::all() {
                        let script = format!("ulimit -c 0; {output} kill -{signal} $$");
                        let result = shell_result(&script, stdout, stderr);
                        assert!(!result.0, "{signal}, {stdout}, {stderr}");
                        assert_eq!(result.1, 128 + signal, "{stdout}, {stderr}");
                    }
                }
            }
        }
    }

    #[test]
    fn ordinary_exit_status_does_not_depend_on_error_output_or_stdio_mode() {
        for code in [0, 7] {
            for output in ["", "printf 'Child diagnostic.\\n' >&2;"] {
                for &stdout in ProcessSpawnOptionsStdioKind::all() {
                    for &stderr in ProcessSpawnOptionsStdioKind::all() {
                        let result = shell_result(&format!("{output} exit {code}"), stdout, stderr);
                        assert_eq!(result, (code == 0, code));
                    }
                }
            }
        }
    }
}
