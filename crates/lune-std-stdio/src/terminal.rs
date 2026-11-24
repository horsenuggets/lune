#[cfg(unix)]
use std::{
    collections::VecDeque,
    io::{self, Read},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(unix)]
use crate::terminal_replies::ReplyFilter;

#[cfg(unix)]
static PENDING_INPUT: LazyLock<Arc<Mutex<VecDeque<u8>>>> =
    LazyLock::new(|| Arc::new(Mutex::default()));
#[cfg(unix)]
static REPLIES: LazyLock<Arc<ReplyFilter>> = LazyLock::new(|| Arc::new(ReplyFilter::default()));

pub(crate) fn prompt_terminal() -> console::Term {
    let terminal = console::Term::stderr();
    #[cfg(unix)]
    {
        terminal.with_filtered_input(Arc::clone(&PENDING_INPUT), REPLIES.clone())
    }
    #[cfg(not(unix))]
    {
        terminal
    }
}

/// Bytes typed during a query remain available to subsequent stdio reads.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct Input<R> {
    inner: R,
    pending: Arc<Mutex<VecDeque<u8>>>,
    replies: Arc<ReplyFilter>,
}

#[cfg(unix)]
impl<R> Input<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            pending: Arc::clone(&PENDING_INPUT),
            replies: Arc::clone(&REPLIES),
        }
    }
}

#[cfg(unix)]
impl<R: Read> Read for Input<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !pending.is_empty() {
                let count = pending.len().min(output.len());
                for byte in &mut output[..count] {
                    *byte = pending
                        .pop_front()
                        .expect("Pending input length was checked.");
                }
                return Ok(count);
            }
            drop(pending);
            let mut raw = [0; 1024];
            let count = match self.inner.read(&mut raw) {
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut pending = self
                        .pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    self.replies.flush_pending(&mut pending);
                    if pending.is_empty() {
                        return Err(error);
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if count == 0 {
                self.replies.flush_pending(&mut pending);
                if pending.is_empty() {
                    return Ok(0);
                }
            } else {
                self.replies.process(&raw[..count], &mut pending);
            }
        }
    }
}

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct ReadCancellation(Arc<AtomicBool>);

#[cfg(unix)]
impl ReadCancellation {
    pub(crate) fn token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }

    pub(crate) fn monitor(&self, lua: &mlua::Lua, cancel: impl FnOnce() + 'static) {
        use mlua_luau_scheduler::LuaSpawnExt;

        let thread = lua.current_thread();
        let cancelled = self.token();
        // coroutine.close can retain the native future, so observe its owner separately.
        lua.spawn_local(async move {
            while !cancelled.load(Ordering::Acquire)
                && thread.status() == mlua::thread::ThreadStatus::Resumable
            {
                blocking::unblock(|| std::thread::sleep(std::time::Duration::from_millis(25)))
                    .await;
            }
            cancelled.store(true, Ordering::Release);
            cancel();
        });
    }
}

#[cfg(unix)]
impl Drop for ReadCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(unix)]
pub(crate) struct ReadTransaction<R> {
    input: Input<R>,
    consumed: Vec<u8>,
    cancelled: Arc<AtomicBool>,
}

#[cfg(unix)]
impl<R> ReadTransaction<R> {
    pub(crate) fn commit(&mut self) {
        self.consumed.clear();
    }
}

#[cfg(unix)]
impl<R: Read> Read for ReadTransaction<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(io::Error::other("Standard input read was cancelled."));
            }
            match self.input.read(output) {
                Ok(count) => {
                    self.consumed.extend_from_slice(&output[..count]);
                    return Ok(count);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(unix)]
impl<R> Drop for ReadTransaction<R> {
    fn drop(&mut self) {
        let mut pending = self
            .input
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for byte in self.consumed.drain(..).rev() {
            pending.push_front(byte);
        }
    }
}

#[cfg(unix)]
pub(crate) struct StdinSource {
    cancelled: Arc<AtomicBool>,
}

#[cfg(unix)]
impl Read for StdinSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        // Bypass Stdin's global read-ahead buffer, which console prompts cannot see.
        read_available(&std::io::stdin(), &self.cancelled, output)
    }
}

#[cfg(unix)]
fn read_available(
    input: &impl std::os::fd::AsFd,
    cancelled: &AtomicBool,
    output: &mut [u8],
) -> io::Result<usize> {
    use nix::poll::{PollFd, PollFlags, poll};

    let mut descriptors = [PollFd::new(input.as_fd(), PollFlags::POLLIN)];
    if poll(&mut descriptors, 25_u16)? == 0 {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::other("Standard input read was cancelled."));
    }
    nix::unistd::read(input, output).map_err(Into::into)
}

#[cfg(unix)]
fn read_line_from(input: &mut impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0];
    loop {
        match input.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(unix)]
pub(crate) fn input_transaction(cancelled: Arc<AtomicBool>) -> ReadTransaction<StdinSource> {
    ReadTransaction {
        input: Input::new(StdinSource {
            cancelled: Arc::clone(&cancelled),
        }),
        consumed: Vec::new(),
        cancelled,
    }
}

#[cfg(unix)]
pub(crate) fn read_line(input: &mut ReadTransaction<StdinSource>) -> io::Result<String> {
    read_line_from(input)
}

#[cfg(unix)]
pub(crate) fn read_to_end(input: &mut ReadTransaction<StdinSource>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn cached_foreground_opacity() -> Option<bool> {
    #[cfg(unix)]
    {
        unix::cached_foreground_opacity()
    }
    #[cfg(not(unix))]
    {
        Some(false)
    }
}

pub(crate) fn foreground_opacity() -> bool {
    #[cfg(unix)]
    {
        unix::foreground_opacity()
    }
    #[cfg(not(unix))]
    {
        // Console input requires a separate event-preserving backend on Windows.
        false
    }
}

#[cfg(unix)]
mod unix {
    use std::{
        fs::{File, OpenOptions},
        io::{self, IsTerminal, Read, Write},
        os::{fd::AsFd, unix::fs::OpenOptionsExt},
        sync::{
            Mutex,
            atomic::{AtomicI32, Ordering},
        },
        time::{Duration, Instant},
    };

    use nix::{
        errno::Errno,
        fcntl::OFlag,
        poll::{PollFd, PollFlags, PollTimeout, poll},
        sys::{
            signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal},
            stat::fstat,
            termios::{self, LocalFlags, SetArg, SpecialCharacterIndices, Termios},
        },
        unistd::{getpgrp, tcgetpgrp, ttyname},
    };

    use super::{PENDING_INPUT, ReplyFilter};

    const QUERY: &[u8] = b"\x1bP$qm\x1b\\";
    const TEST_FOREGROUND: &[u8] = b"\x1b[38:6::1:2:3:128m";
    const TEST_PARAMETERS: &str = "38:6::1:2:3:128";
    const MAX_REPLY: usize = 1024;
    const QUERY_TIMEOUT: Duration = Duration::from_millis(100);
    static FOREGROUND_OPACITY: Mutex<Option<(u64, bool)>> = Mutex::new(None);
    static INTERRUPTED: AtomicI32 = AtomicI32::new(0);

    extern "C" fn record_signal(signal: nix::libc::c_int) {
        let _ = INTERRUPTED.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
    }

    struct SignalGuard {
        saved: Vec<(Signal, SigAction)>,
    }

    impl SignalGuard {
        fn new() -> io::Result<Self> {
            INTERRUPTED.store(0, Ordering::SeqCst);
            let mut guard = Self { saved: Vec::new() };
            let action = SigAction::new(
                SigHandler::Handler(record_signal),
                SaFlags::empty(),
                SigSet::empty(),
            );
            for signal in [
                Signal::SIGHUP,
                Signal::SIGINT,
                Signal::SIGQUIT,
                Signal::SIGTERM,
            ] {
                // The handler only records a lock-free atomic. Input ownership prevents
                // concurrent queries from changing these temporary handlers.
                let previous = unsafe { signal::sigaction(signal, &action) }?;
                guard.saved.push((signal, previous));
            }
            Ok(guard)
        }
    }

    impl Drop for SignalGuard {
        fn drop(&mut self) {
            for (signal, previous) in self.saved.iter().rev() {
                // Restore each original handler before delivering the deferred signal.
                let _ = unsafe { signal::sigaction(*signal, previous) };
            }
            if let Ok(signal) = Signal::try_from(INTERRUPTED.swap(0, Ordering::SeqCst)) {
                let _ = signal::raise(signal);
            }
        }
    }

    fn terminal_id() -> Option<u64> {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return None;
        }
        let input = fstat(std::io::stdin()).ok()?;
        let output = fstat(std::io::stdout()).ok()?;
        (input.st_rdev == output.st_rdev).then_some(input.st_rdev as u64)
    }

    pub(super) fn cached_foreground_opacity() -> Option<bool> {
        let Some(device) = terminal_id() else {
            return Some(false);
        };
        let cached = FOREGROUND_OPACITY.try_lock().ok()?;
        cached
            .filter(|(previous, _)| *previous == device)
            .map(|(_, supported)| supported)
    }

    pub(super) fn foreground_opacity() -> bool {
        let Some(device) = terminal_id() else {
            return false;
        };
        if let Some(cached) = cached_foreground_opacity() {
            return cached;
        }
        let supported = probe_current_terminal();
        *FOREGROUND_OPACITY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((device, supported));
        supported
    }

    fn probe_current_terminal() -> bool {
        let Ok(path) = ttyname(std::io::stdin()) else {
            return false;
        };
        let Ok(mut terminal) = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(OFlag::O_NONBLOCK.bits())
            .open(path)
        else {
            return false;
        };
        // Do not query a background job or a different terminal from the one the app
        // reads and writes. A redirected standard stream never receives probe bytes.
        if tcgetpgrp(&terminal).ok() != Some(getpgrp())
            || !same_terminal(&terminal, &std::io::stdin())
            || !same_terminal(&terminal, &std::io::stdout())
        {
            return false;
        }
        // Standard output locks also cover print/warn and asynchronous stdio writers,
        // keeping application rendition changes outside the capability exchange.
        let _output = std::io::stdout().lock();
        let _error = std::io::stderr().lock();
        let Ok(_signals) = SignalGuard::new() else {
            return false;
        };
        let mut pending = Vec::new();
        let result =
            probe(&mut terminal, &mut pending, QUERY_TIMEOUT, &super::REPLIES).unwrap_or(false);
        PENDING_INPUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(pending);
        result
    }

    fn same_terminal(left: &impl AsFd, right: &impl AsFd) -> bool {
        match (fstat(left), fstat(right)) {
            (Ok(left), Ok(right)) => left.st_rdev == right.st_rdev,
            _ => false,
        }
    }

    struct ModeGuard {
        terminal: File,
        saved: Termios,
    }

    impl ModeGuard {
        fn new(terminal: &File) -> io::Result<Self> {
            let saved = termios::tcgetattr(terminal)?;
            let mut querying = saved.clone();
            querying
                .local_flags
                .remove(LocalFlags::ICANON | LocalFlags::ECHO);
            querying.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
            querying.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
            let guard = Self {
                terminal: terminal.try_clone()?,
                saved,
            };
            termios::tcsetattr(terminal, SetArg::TCSANOW, &querying)?;
            Ok(guard)
        }
    }

    impl Drop for ModeGuard {
        fn drop(&mut self) {
            let _ = termios::tcsetattr(&self.terminal, SetArg::TCSANOW, &self.saved);
        }
    }

    struct RenditionGuard {
        terminal: File,
        restore: Vec<u8>,
        armed: bool,
    }

    impl Drop for RenditionGuard {
        fn drop(&mut self) {
            if self.armed {
                let _ = self.terminal.write_all(&self.restore);
                let _ = self.terminal.flush();
            }
        }
    }

    /// A single complete DECRQSS response and all unrelated bytes received around it.
    struct Reply {
        parameters: Option<String>,
        end: usize,
        start: usize,
    }

    fn reply(bytes: &[u8]) -> Option<Reply> {
        for start in 0..bytes.len() {
            let rest = &bytes[start..];
            if !rest.starts_with(b"\x1bP1$r") && !rest.starts_with(b"\x1bP0$r") {
                continue;
            }
            let prefix = 5;
            let payload = &rest[prefix..];
            let end = payload.windows(2).position(|part| part == b"\x1b\\")?;
            let body = &payload[..end];
            let parameters = if rest[2] == b'1'
                && body.ends_with(b"m")
                && body[..body.len() - 1]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':'))
            {
                String::from_utf8(body[..body.len() - 1].to_vec()).ok()
            } else {
                None
            };
            return Some(Reply {
                parameters,
                start,
                end: start + prefix + end + 2,
            });
        }
        None
    }

    fn preserve_late(bytes: &[u8], pending: &mut Vec<u8>, filter: &ReplyFilter) {
        let mut clean = std::collections::VecDeque::new();
        filter.process(bytes, &mut clean);
        // The query wait has ended. Abandon a recognized truncated reply now so
        // immediately following keyboard input cannot become its payload.
        filter.flush_pending(&mut clean);
        pending.extend(clean);
    }

    fn read_reply(
        terminal: &mut File,
        pending: &mut Vec<u8>,
        timeout: Duration,
        filter: &ReplyFilter,
    ) -> io::Result<Option<String>> {
        if filter.has_pending_reply() {
            return Ok(None);
        }
        let deadline = Instant::now() + timeout;
        let mut bytes = Vec::new();
        let mut filtering = false;
        let result = loop {
            if INTERRUPTED.load(Ordering::SeqCst) != 0 {
                break Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if !filtering {
                if let Some(response) = reply(&bytes) {
                    pending.extend_from_slice(&bytes[..response.start]);
                    pending.extend_from_slice(&bytes[response.end..]);
                    return Ok(response.parameters);
                }
                if bytes.len() >= MAX_REPLY {
                    // Keep draining an oversized reply within the original deadline.
                    // Returning at the size limit would strand its tail in stdin.
                    filter.expect_reply();
                    filtering = true;
                    let mut clean = std::collections::VecDeque::new();
                    filter.process(&bytes, &mut clean);
                    pending.extend(clean);
                    bytes.clear();
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break Ok(None);
            }
            let mut descriptors = [PollFd::new(terminal.as_fd(), PollFlags::POLLIN)];
            let wait = PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX);
            match poll(&mut descriptors, wait) {
                Err(Errno::EINTR) | Ok(0) => continue,
                Err(error) => break Err(error.into()),
                Ok(_) => {}
            }
            if !descriptors[0]
                .revents()
                .unwrap_or(PollFlags::empty())
                .contains(PollFlags::POLLIN)
            {
                break Ok(None);
            }
            let mut buffer = [0; 128];
            let available = if filtering {
                buffer.len()
            } else {
                buffer.len().min(MAX_REPLY - bytes.len())
            };
            match terminal.read(&mut buffer[..available]) {
                Ok(0) => break Ok(None),
                Ok(count) if filtering => {
                    let mut clean = std::collections::VecDeque::new();
                    filter.process(&buffer[..count], &mut clean);
                    pending.extend(clean);
                }
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => break Err(error),
            }
        };
        if !filtering {
            filter.expect_reply();
        }
        preserve_late(&bytes, pending, filter);
        result
    }

    fn probe(
        terminal: &mut File,
        pending: &mut Vec<u8>,
        timeout: Duration,
        filter: &ReplyFilter,
    ) -> io::Result<bool> {
        let _mode = ModeGuard::new(terminal)?;
        // Obtain the exact rendition before changing anything. Silence, an unsupported
        // DECRQSS response, or malformed parameters stop here without changing colors.
        terminal.write_all(QUERY)?;
        terminal.flush()?;
        let Some(original) = read_reply(terminal, pending, timeout, filter)? else {
            return Ok(false);
        };
        let restore = format!("\x1b[0;{original}m").into_bytes();
        let mut rendition = RenditionGuard {
            terminal: terminal.try_clone()?,
            restore: restore.clone(),
            armed: true,
        };
        let mut request = TEST_FOREGROUND.to_vec();
        request.extend_from_slice(QUERY);
        // The terminal restores its rendition immediately after composing the reply,
        // even if that reply never arrives or the waiting task is cancelled.
        request.extend_from_slice(&restore);
        terminal.write_all(&request)?;
        terminal.flush()?;
        rendition.armed = false;
        let Some(parameters) = read_reply(terminal, pending, timeout, filter)? else {
            return Ok(false);
        };
        Ok(parameters.split(';').any(|value| value == TEST_PARAMETERS))
    }

    #[cfg(test)]
    mod tests {
        use std::{
            io::{Read, Write},
            thread,
        };

        use nix::pty::openpty;

        use super::*;

        fn probe(
            terminal: &mut File,
            pending: &mut Vec<u8>,
            timeout: Duration,
        ) -> io::Result<bool> {
            super::probe(terminal, pending, timeout, &ReplyFilter::default())
        }

        fn pair() -> (File, File) {
            let pty = openpty(None, None).expect("A PTY must be available.");
            (File::from(pty.master), File::from(pty.slave))
        }

        fn assert_mode(terminal: &File, expected: &Termios) {
            let actual = termios::tcgetattr(terminal).unwrap();
            assert_eq!(actual.input_flags, expected.input_flags);
            assert_eq!(actual.output_flags, expected.output_flags);
            assert_eq!(actual.control_flags, expected.control_flags);
            // The kernel sets PENDIN when returning to canonical mode. It is pending
            // input bookkeeping, not an application-selected terminal mode.
            assert_eq!(
                actual.local_flags & !LocalFlags::PENDIN,
                expected.local_flags & !LocalFlags::PENDIN
            );
            assert_eq!(actual.control_chars, expected.control_chars);
            assert_eq!(
                termios::cfgetispeed(&actual),
                termios::cfgetispeed(expected)
            );
            assert_eq!(
                termios::cfgetospeed(&actual),
                termios::cfgetospeed(expected)
            );
        }

        fn read_until(terminal: &mut File, suffix: &[u8]) -> Vec<u8> {
            let mut result = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !result.ends_with(suffix) {
                assert!(
                    Instant::now() < deadline,
                    "The test terminal did not receive its request."
                );
                let mut descriptors = [PollFd::new(terminal.as_fd(), PollFlags::POLLIN)];
                if poll(&mut descriptors, 100u16).unwrap() == 0 {
                    continue;
                }
                let mut byte = [0];
                terminal.read_exact(&mut byte).unwrap();
                result.push(byte[0]);
            }
            result
        }

        #[test]
        fn parses_only_complete_numeric_sgr_replies() {
            let valid = reply(b"typed\x1bP1$r0;38:6::1:2:3:128m\x1b\\after").unwrap();
            assert_eq!(valid.parameters.as_deref(), Some("0;38:6::1:2:3:128"));
            assert_eq!(valid.start, 5);
            assert!(reply(b"\x1bP1$r0m\x1b").is_none());
            assert!(reply(b"\x1bP0$rm\x1b\\").unwrap().parameters.is_none());
            assert!(reply(b"\x1bP1$r0;badm\x1b\\").unwrap().parameters.is_none());
            assert!(reply(b"\x1bP1$r0q\x1b\\").unwrap().parameters.is_none());
        }

        #[test]
        fn accepts_exact_alpha_and_restores_rendition_and_mode() {
            let (mut master, mut slave) = pair();
            let saved = termios::tcgetattr(&slave).unwrap();
            let emulator = thread::spawn(move || {
                assert_eq!(read_until(&mut master, QUERY), QUERY);
                master
                    .write_all(b"\x1bP1$r0;1;38:2::50:60:70m\x1b\\")
                    .unwrap();
                let request = read_until(&mut master, b"\x1b[0;0;1;38:2::50:60:70m");
                assert!(request.starts_with(TEST_FOREGROUND));
                assert!(request.windows(QUERY.len()).any(|part| part == QUERY));
                // Fragmenting a reply must not turn it into an unsupported response.
                master.write_all(b"\x1bP1$r0;38:6::1:").unwrap();
                thread::sleep(Duration::from_millis(5));
                master.write_all(b"2:3:128m\x1b\\").unwrap();
                master
            });
            let mut pending = Vec::new();
            assert!(probe(&mut slave, &mut pending, QUERY_TIMEOUT).unwrap());
            assert!(pending.is_empty());
            assert_mode(&slave, &saved);
            let master = emulator.join().unwrap();
            let mut descriptors = [PollFd::new(master.as_fd(), PollFlags::POLLIN)];
            assert_eq!(poll(&mut descriptors, PollTimeout::ZERO).unwrap(), 0);
        }

        #[test]
        fn rejects_unsupported_wrong_alpha_and_rgb_only() {
            for response in [
                b"\x1bP0$rm\x1b\\".as_slice(),
                b"\x1bP1$r0;38:6::1:2:3:255m\x1b\\".as_slice(),
                b"\x1bP1$r0;38:2::1:2:3m\x1b\\".as_slice(),
                b"\x1bP1$r0;38:6::4:2:3:128m\x1b\\".as_slice(),
                b"\x1bP1$r0;38:6::1:2:3:1280m\x1b\\".as_slice(),
            ] {
                let (mut master, mut slave) = pair();
                let saved = termios::tcgetattr(&slave).unwrap();
                let response = response.to_vec();
                let emulator = thread::spawn(move || {
                    read_until(&mut master, QUERY);
                    master.write_all(b"\x1bP1$r0m\x1b\\").unwrap();
                    read_until(&mut master, b"\x1b[0;0m");
                    master.write_all(&response).unwrap();
                    master
                });
                assert!(!probe(&mut slave, &mut Vec::new(), QUERY_TIMEOUT).unwrap());
                assert_mode(&slave, &saved);
                drop(emulator.join().unwrap());
            }
        }

        #[test]
        fn silent_terminal_times_out_without_changing_rendition_or_mode() {
            let (mut master, mut slave) = pair();
            let saved = termios::tcgetattr(&slave).unwrap();
            let start = Instant::now();
            assert!(!probe(&mut slave, &mut Vec::new(), Duration::from_millis(20)).unwrap());
            assert!(start.elapsed() < Duration::from_secs(1));
            assert_mode(&slave, &saved);
            assert_eq!(read_until(&mut master, QUERY), QUERY);
        }

        #[test]
        fn partial_reply_times_out_and_preserves_typed_input() {
            let (mut master, mut slave) = pair();
            let saved = termios::tcgetattr(&slave).unwrap();
            let emulator = thread::spawn(move || {
                read_until(&mut master, QUERY);
                master.write_all(b"typed\x1b[A\x1bP1$r0;").unwrap();
                // Keep the PTY open until the probe has timed out and restored its mode.
                thread::sleep(Duration::from_millis(100));
                master
            });
            let mut pending = Vec::new();
            let filter = ReplyFilter::default();
            assert!(
                !super::probe(&mut slave, &mut pending, Duration::from_millis(20), &filter)
                    .unwrap()
            );
            assert_eq!(pending, b"typed\x1b[A");
            assert_mode(&slave, &saved);
            assert!(!filter.has_pending_reply());
            let mut clean = std::collections::VecDeque::new();
            filter.process(b"12345\nhello\n", &mut clean);
            assert_eq!(clean.into_iter().collect::<Vec<_>>(), b"12345\nhello\n");
            drop(emulator.join().unwrap());
        }

        #[test]
        fn oversized_reply_is_drained_within_the_original_deadline() {
            let (mut master, mut slave) = pair();
            let saved = termios::tcgetattr(&slave).unwrap();
            let emulator = thread::spawn(move || {
                read_until(&mut master, QUERY);
                master.write_all(b"\x1bP1$r").unwrap();
                master.write_all(&vec![b'1'; MAX_REPLY * 2]).unwrap();
                master
            });
            let filter = ReplyFilter::default();
            let mut pending = Vec::new();
            let started = Instant::now();
            assert!(!super::probe(&mut slave, &mut pending, QUERY_TIMEOUT, &filter).unwrap());
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(pending.is_empty());
            assert!(!filter.has_pending_reply());
            assert_mode(&slave, &saved);
            let mut master = emulator.join().unwrap();
            master.write_all(b"12345\n").unwrap();
            let mut input = [0; 6];
            slave.read_exact(&mut input).unwrap();
            assert_eq!(&input, b"12345\n");
        }

        #[test]
        fn test_reply_timeout_restores_original_rendition() {
            let (mut master, mut slave) = pair();
            let saved = termios::tcgetattr(&slave).unwrap();
            let emulator = thread::spawn(move || {
                read_until(&mut master, QUERY);
                master.write_all(b"\x1bP1$r0;3m\x1b\\").unwrap();
                let request = read_until(&mut master, b"\x1b[0;0;3m");
                assert!(request.starts_with(TEST_FOREGROUND));
                master
            });
            assert!(!probe(&mut slave, &mut Vec::new(), Duration::from_millis(20)).unwrap());
            assert_mode(&slave, &saved);
            drop(emulator.join().unwrap());
        }

        #[test]
        fn preserves_input_surrounding_a_complete_reply() {
            let (mut master, mut slave) = pair();
            let emulator = thread::spawn(move || {
                read_until(&mut master, QUERY);
                master.write_all(b"before\x1bP1$r0m\x1b\\after").unwrap();
                read_until(&mut master, b"\x1b[0;0m");
                master
                    .write_all(b"\x1bP1$r0;38:6::1:2:3:128m\x1b\\")
                    .unwrap();
                master
            });
            let mut pending = Vec::new();
            assert!(probe(&mut slave, &mut pending, QUERY_TIMEOUT).unwrap());
            assert_eq!(pending, b"beforeafter");
            drop(emulator.join().unwrap());
        }

        #[test]
        fn a_nonterminal_cannot_enter_query_mode() {
            let (read, _) = nix::unistd::pipe().unwrap();
            let mut input = File::from(read);
            assert!(probe(&mut input, &mut Vec::new(), QUERY_TIMEOUT).is_err());
        }

        struct ProbeChild(std::process::Child);

        impl Drop for ProbeChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        #[test]
        fn signal_probe_child() {
            extern "C" fn observe_restored_mode(signal: nix::libc::c_int) {
                let mut mode = std::mem::MaybeUninit::<nix::libc::termios>::uninit();
                // Observe mode before the session leader exits and Darwin revokes the
                // slave descriptor. Only async-signal-safe system calls run here.
                unsafe {
                    let restored = nix::libc::tcgetattr(0, mode.as_mut_ptr()) == 0
                        && mode.assume_init().c_lflag & (nix::libc::ICANON | nix::libc::ECHO)
                            == (nix::libc::ICANON | nix::libc::ECHO);
                    nix::libc::_exit(if restored { 128 + signal } else { 1 });
                }
            }
            if std::env::var_os("LUNE_TERMINAL_SIGNAL_CHILD").is_some() {
                let action = SigAction::new(
                    SigHandler::Handler(observe_restored_mode),
                    SaFlags::empty(),
                    SigSet::empty(),
                );
                for signal in [
                    Signal::SIGHUP,
                    Signal::SIGINT,
                    Signal::SIGQUIT,
                    Signal::SIGTERM,
                ] {
                    // The subprocess owns these test-only signal handlers.
                    unsafe { signal::sigaction(signal, &action) }.unwrap();
                }
                foreground_opacity();
                panic!("The terminal query must deliver the deferred signal.");
            }
        }

        #[test]
        fn restores_terminal_mode_before_delivering_termination_signals() {
            use std::os::fd::AsRawFd;
            use std::os::unix::process::CommandExt;
            use std::process::{Command, Stdio};

            for phase in [1, 2] {
                for signal in [
                    Signal::SIGHUP,
                    Signal::SIGINT,
                    Signal::SIGQUIT,
                    Signal::SIGTERM,
                ] {
                    let (mut master, slave) = pair();
                    let mut command = Command::new(std::env::current_exe().unwrap());
                    command
                        .args([
                            "--exact",
                            "terminal::unix::tests::signal_probe_child",
                            "--nocapture",
                        ])
                        .env("LUNE_TERMINAL_SIGNAL_CHILD", "1")
                        .stdin(Stdio::from(slave.try_clone().unwrap()))
                        .stdout(Stdio::from(slave.try_clone().unwrap()))
                        .stderr(Stdio::from(slave.try_clone().unwrap()));
                    // The child only establishes its controlling terminal before exec.
                    unsafe {
                        command.pre_exec(|| {
                            nix::unistd::setsid()?;
                            if nix::libc::ioctl(
                                std::io::stdin().as_raw_fd(),
                                nix::libc::TIOCSCTTY as _,
                                0,
                            ) < 0
                            {
                                return Err(io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                    let mut child = ProbeChild(command.spawn().unwrap());
                    read_until(&mut master, QUERY);
                    if phase == 2 {
                        master.write_all(b"\x1bP1$r0m\x1b\\").unwrap();
                        read_until(&mut master, b"\x1b[0;0m");
                    }
                    signal::kill(nix::unistd::Pid::from_raw(child.0.id() as i32), signal).unwrap();
                    let status = child.0.wait().unwrap();
                    assert_eq!(status.code(), Some(128 + signal as i32));
                }
            }
        }

        #[test]
        fn different_terminal_devices_do_not_match() {
            let (_, first) = pair();
            let (_, second) = pair();
            assert!(!same_terminal(&first, &second));
            assert!(same_terminal(&first, &first));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{os::fd::OwnedFd, sync::mpsc, time::Duration};

    struct PipeSource {
        descriptor: OwnedFd,
        cancelled: Arc<AtomicBool>,
        consumed: mpsc::Sender<()>,
    }

    impl Read for PipeSource {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = read_available(&self.descriptor, &self.cancelled, output)?;
            if count > 0 {
                self.consumed.send(()).unwrap();
            }
            Ok(count)
        }
    }

    fn transaction<R>(inner: R, cancelled: Arc<AtomicBool>) -> ReadTransaction<R> {
        ReadTransaction {
            input: Input {
                inner,
                pending: Arc::new(Mutex::default()),
                replies: Arc::new(ReplyFilter::default()),
            },
            consumed: Vec::new(),
            cancelled,
        }
    }

    #[test]
    fn cancelled_reads_release_ownership_without_eof_and_restore_input() {
        for to_end in [false, true] {
            for bytes in [b"".as_slice(), b"partial", b"first\nsecond\n"] {
                let cancellation = ReadCancellation::default();
                let (reader, writer) = nix::unistd::pipe().unwrap();
                let replacement = reader.try_clone().unwrap();
                let (consumed, received) = mpsc::channel();
                let mut transaction = transaction(
                    PipeSource {
                        descriptor: reader,
                        cancelled: cancellation.token(),
                        consumed,
                    },
                    cancellation.token(),
                );
                let pending = Arc::clone(&transaction.input.pending);
                let gate = Arc::new(async_lock::Mutex::new(()));
                let ownership = futures_lite::future::block_on(gate.lock_arc());
                let (finished, completion) = mpsc::channel();
                let worker = std::thread::spawn(move || {
                    let result = if to_end {
                        let mut bytes = Vec::new();
                        transaction.read_to_end(&mut bytes).map(|_| ())
                    } else {
                        read_line_from(&mut transaction).map(|_| ())
                    };
                    // Model a result discarded after the awaiter has been cancelled.
                    drop((transaction, result, ownership));
                    finished.send(()).unwrap();
                });
                if !bytes.is_empty() {
                    nix::unistd::write(&writer, bytes).unwrap();
                    received.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                drop(cancellation);
                completion.recv_timeout(Duration::from_secs(2)).unwrap();
                worker.join().unwrap();
                assert!(gate.try_lock().is_some());
                assert_eq!(
                    pending.lock().unwrap().iter().copied().collect::<Vec<_>>(),
                    bytes
                );
                // The pipe's write end stays open throughout cancellation.
                nix::unistd::write(&writer, b"replacement\n").unwrap();
                let mut input = Input {
                    inner: std::fs::File::from(replacement),
                    pending,
                    replies: Arc::new(ReplyFilter::default()),
                };
                let mut restored = vec![0; bytes.len() + b"replacement\n".len()];
                input.read_exact(&mut restored).unwrap();
                assert_eq!(restored, [bytes, b"replacement\n"].concat());
            }
        }
    }

    #[test]
    fn closed_luau_owners_release_undelivered_input_before_the_worker_returns() {
        use mlua_luau_scheduler::{Functions, LuaSpawnExt, Scheduler};

        let lua = mlua::Lua::new();
        let scheduler = Scheduler::new(lua.clone());
        let functions = Functions::new(lua.clone()).unwrap();
        lua.globals().set("spawn", functions.spawn).unwrap();
        lua.globals().set("cancel", functions.cancel).unwrap();
        lua.globals()
            .set(
                "pause",
                lua.create_async_function(|_, ()| async {
                    blocking::unblock(|| std::thread::sleep(Duration::from_millis(5))).await;
                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();
        let mut transaction = transaction(io::Cursor::new(b"first\nsecond\n"), Arc::default());
        let result = read_line_from(&mut transaction).unwrap();
        let pending = Arc::clone(&transaction.input.pending);
        let gate = Arc::new(async_lock::Mutex::new(()));
        let ownership = futures_lite::future::block_on(gate.lock_arc());
        let delivery = Arc::new(Mutex::new(Some((transaction, result, ownership))));
        let observed = Arc::new(AtomicBool::default());
        let worker_observed = Arc::clone(&observed);
        let worker_gate = Arc::clone(&gate);
        lua.globals()
            .set(
                "read",
                lua.create_async_function(move |lua, ()| {
                    let abandoned = Arc::clone(&delivery);
                    let observed = Arc::clone(&worker_observed);
                    let gate = Arc::clone(&worker_gate);
                    async move {
                        lua.globals().set("started", true)?;
                        let cancellation = ReadCancellation::default();
                        let cancelled = cancellation.token();
                        cancellation.monitor(&lua, move || {
                            abandoned.lock().unwrap().take();
                        });
                        lua.spawn_blocking(move || {
                            // The owner must be detected independently of native completion.
                            for _ in 0..200 {
                                if cancelled.load(Ordering::Acquire) && gate.try_lock().is_some() {
                                    observed.store(true, Ordering::Release);
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        })
                        .await;
                        Ok(())
                    }
                })
                .unwrap(),
            )
            .unwrap();
        scheduler
            .push_thread_front(lua.load("local owner = spawn(function() read() end); repeat pause() until started; cancel(owner)"), ())
            .unwrap();
        futures_lite::future::block_on(scheduler.run());
        assert!(observed.load(Ordering::Acquire));
        assert!(gate.try_lock().is_some());
        assert_eq!(
            pending.lock().unwrap().make_contiguous(),
            b"first\nsecond\n"
        );
    }

    #[test]
    fn cancelled_reads_leave_queued_input_untouched() {
        let cancellation = ReadCancellation::default();
        let mut transaction = transaction(io::Cursor::new(b"later"), cancellation.token());
        transaction.input.pending.lock().unwrap().extend(b"first");
        drop(cancellation);
        assert!(transaction.read(&mut [0; 16]).is_err());
        assert!(transaction.consumed.is_empty());
        assert_eq!(transaction.input.inner.position(), 0);
        assert_eq!(
            transaction.input.pending.lock().unwrap().make_contiguous(),
            b"first"
        );
    }

    #[test]
    fn undelivered_results_restore_bytes_before_releasing_ownership() {
        struct Ownership {
            pending: Arc<Mutex<VecDeque<u8>>>,
        }
        impl Drop for Ownership {
            fn drop(&mut self) {
                assert_eq!(
                    self.pending.lock().unwrap().make_contiguous(),
                    b"first\nsecond\n"
                );
            }
        }

        let mut transaction = transaction(io::Cursor::new(b"first\nsecond\n"), Arc::default());
        let result = read_line_from(&mut transaction).unwrap();
        assert_eq!(result, "first\n");
        let ownership = Ownership {
            pending: Arc::clone(&transaction.input.pending),
        };
        drop((transaction, result, ownership));
    }

    #[test]
    fn committed_results_keep_only_unconsumed_input() {
        let mut transaction = transaction(io::Cursor::new(b"first\nsecond\n"), Arc::default());
        assert_eq!(read_line_from(&mut transaction).unwrap(), "first\n");
        let pending = Arc::clone(&transaction.input.pending);
        transaction.commit();
        drop(transaction);
        assert_eq!(pending.lock().unwrap().make_contiguous(), b"second\n");
    }

    #[test]
    fn rollback_does_not_restore_filtered_terminal_replies() {
        let mut transaction = transaction(
            io::Cursor::new(b"\x1bP1$r0m\x1b\\first\nsecond\n"),
            Arc::default(),
        );
        transaction.input.replies.expect_reply();
        assert_eq!(read_line_from(&mut transaction).unwrap(), "first\n");
        let pending = Arc::clone(&transaction.input.pending);
        drop(transaction);
        assert_eq!(
            pending.lock().unwrap().make_contiguous(),
            b"first\nsecond\n"
        );
    }

    #[test]
    fn quiet_reads_release_pending_reply_filter_input() {
        struct Quiet;
        impl Read for Quiet {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }

        let replies = Arc::new(ReplyFilter::default());
        replies.expect_reply();
        // A quiet interval is distinct from EOF and must run the same recovery hook.
        replies.process(b"\x1b", &mut VecDeque::new());
        let mut quiet = Input {
            inner: Quiet,
            pending: Arc::new(Mutex::default()),
            replies,
        };
        let mut byte = [0];
        assert_eq!(quiet.read(&mut byte).unwrap(), 1);
        assert_eq!(byte, [0x1b]);
    }

    #[test]
    fn reads_preserved_input_before_the_underlying_stream() {
        let mut input = Input {
            inner: io::Cursor::new(b"later"),
            pending: Arc::new(Mutex::new(VecDeque::from(b"typed".to_vec()))),
            replies: Arc::new(ReplyFilter::default()),
        };
        let mut buffer = [0; 3];
        assert_eq!(input.read(&mut []).unwrap(), 0);
        assert_eq!(input.read(&mut buffer).unwrap(), 3);
        assert_eq!(&buffer, b"typ");
        assert_eq!(input.read(&mut buffer).unwrap(), 2);
        assert_eq!(&buffer[..2], b"ed");
        assert_eq!(input.read(&mut buffer).unwrap(), 3);
        assert_eq!(&buffer, b"lat");
    }

    #[test]
    fn reads_lines_without_retaining_later_input_or_reply_fragments() {
        let replies = Arc::new(ReplyFilter::default());
        replies.expect_reply();
        let mut input = Input {
            inner: io::Cursor::new(b"\x1bP1$r0m\x1b\\first\nsecond".to_vec()),
            pending: Arc::new(Mutex::default()),
            replies: Arc::clone(&replies),
        };
        assert_eq!(read_line_from(&mut input).unwrap(), "first\n");
        let mut following_reader = Input {
            inner: io::Cursor::new(b""),
            pending: Arc::clone(&input.pending),
            replies: Arc::clone(&replies),
        };
        let mut rest = String::new();
        following_reader.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "second");
        assert!(!replies.has_pending_reply());
    }
}
