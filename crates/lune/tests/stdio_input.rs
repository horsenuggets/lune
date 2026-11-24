#![cfg(all(unix, feature = "cli", feature = "std-stdio", feature = "std-task"))]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct InputChild(Child);

impl Drop for InputChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn cancelling_a_queued_reader_does_not_strand_input_ownership() {
    for read in ["readLine", "readToEnd"] {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("queued-input.luau");
        fs::write(
            &script,
            r#"
const stdio = require("@lune/stdio")
const task = require("@lune/task")
local ownerStarted = false
local queuedStarted = false
local completed = false
local unexpected = false
task.spawn(function()
    ownerStarted = true
    assert(stdio.readLine() == "first\n")
    completed = true
end)
while not ownerStarted do
    task.wait()
end
const queued = task.spawn(function()
    queuedStarted = true
    stdio.CANCELLED_READ()
    unexpected = true
end)
while not queuedStarted do
    task.wait()
end
task.cancel(queued)
print("Ready for input.")
assert(stdio.readLine() == "second\n")
while not completed do
    task.wait()
end
assert(not unexpected)
print("Queued cancellation completed.")
"#
            .replace("CANCELLED_READ", read),
        )
        .unwrap();
        let mut child = InputChild(
            Command::new(env!("CARGO_BIN_EXE_lune"))
                .arg("run")
                .arg(&script)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let output = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            "Ready for input."
        );
        child
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"first\nsecond\n")
            .unwrap();
        // Keep stdin open. A cancelled waiter must not require EOF to release the gate.
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "The cancelled {read} waiter retained input ownership."
            );
            thread::sleep(Duration::from_millis(5));
        };
        output.join().unwrap();
        assert!(
            status.success(),
            "The {read} cancellation regression failed."
        );
        assert_eq!(
            receiver.into_iter().collect::<Vec<_>>(),
            ["Queued cancellation completed."]
        );
    }
}
