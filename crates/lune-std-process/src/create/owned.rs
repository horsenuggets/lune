//! Opt-in native ownership. Only the worker touches or reaps the direct child.
//! A tiny Unix supervisor reserves the group identity until the final group signal.
//! Windows retains the native job independently of the direct child's exit.

use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use async_channel::{Receiver, Sender, bounded};
use mlua::prelude::*;

use super::{ChildReader, ChildWriter};

#[derive(Debug)]
struct Completion<T> {
    result: Mutex<Option<Result<T, Arc<io::Error>>>>,
    tx: Sender<()>,
    rx: Receiver<()>,
}

impl<T: Clone> Completion<T> {
    fn new() -> Self {
        let (tx, rx) = bounded(1);
        Self {
            result: Mutex::new(None),
            tx,
            rx,
        }
    }

    fn publish(&self, value: io::Result<T>) {
        let mut result = self.result.lock().unwrap();
        if result.is_none() {
            *result = Some(value.map_err(Arc::new));
            self.tx.close();
        }
    }

    fn succeeded(&self) -> bool {
        matches!(self.result.lock().unwrap().as_ref(), Some(Ok(_)))
    }

    async fn wait(&self) -> io::Result<T> {
        // Closing, not sending, broadcasts to every current and future waiter.
        let _ = self.rx.recv().await;
        self.result
            .lock()
            .unwrap()
            .as_ref()
            .expect("published completion")
            .clone()
            .map_err(|error| io::Error::new(error.kind(), error.to_string()))
    }
}

#[derive(Debug)]
enum Request {
    Kill,
    Terminate,
}

#[derive(Debug)]
struct Owner {
    requests: mpsc::Sender<Request>,
    requested: AtomicBool,
    status: Arc<Completion<ExitStatus>>,
    terminated: Arc<Completion<()>>,
    stdin: ChildWriter,
}

impl Owner {
    fn terminate(&self) {
        // No pipe mutex may be held here. A writer can be blocked in the OS until the
        // kill releases the pipe's reader. cancel also prevents subsequent writes.
        self.stdin.cancel();
        if !self.requested.swap(true, Ordering::SeqCst) {
            let _ = self.requests.send(Request::Terminate);
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[derive(Clone, Debug)]
pub(super) struct OwnedProcess(Arc<Owner>);

impl OwnedProcess {
    pub fn spawn(
        mut command: Command,
    ) -> io::Result<(Self, ChildWriter, ChildReader, ChildReader)> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut native = Native::spawn(command)?;
        let (stdin, stdout, stderr) = native.streams();
        let status = Arc::new(Completion::new());
        let terminated = Arc::new(Completion::new());
        let (requests, receiver) = mpsc::channel();
        let owner = Arc::new(Owner {
            requests,
            requested: AtomicBool::new(false),
            status: Arc::clone(&status),
            terminated: Arc::clone(&terminated),
            stdin: stdin.clone(),
        });
        // Independent of Lua futures and of their cancellation. Native's drop guard
        // also cleans up if thread creation fails or the worker unwinds.
        thread::Builder::new()
            .name("lune-owned-process".into())
            .spawn(move || {
                supervise(&mut native, &receiver, &status, &terminated);
            })?;
        Ok((Self(owner), stdin, stdout, stderr))
    }

    pub fn register(&self, lua: &Lua) {
        if lua.app_data_ref::<Registry>().is_none() {
            lua.set_app_data(Registry::default());
        }
        let registry = lua.app_data_ref::<Registry>().unwrap();
        registry.register(Registration {
            owner: Arc::downgrade(&self.0),
            terminated: Arc::clone(&self.0.terminated),
        });
    }

    pub fn kill(&self) {
        let _ = self.0.requests.send(Request::Kill);
    }

    pub async fn status(&self) -> io::Result<ExitStatus> {
        self.0.status.wait().await
    }

    pub async fn terminate(&self) -> io::Result<()> {
        self.0.terminate();
        self.0.terminated.wait().await
    }
}

fn supervise(
    native: &mut Native,
    requests: &mpsc::Receiver<Request>,
    status: &Completion<ExitStatus>,
    terminated: &Completion<()>,
) {
    let mut observed = false;
    loop {
        let request = if observed {
            requests
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            requests.recv_timeout(Duration::from_millis(10))
        };
        match request {
            Ok(Request::Kill) => {
                if !observed {
                    let _ = native.kill_direct();
                }
            }
            Ok(Request::Terminate) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let result = native.terminate();
                match result {
                    Ok(exit) => {
                        status.publish(Ok(exit));
                        terminated.publish(Ok(()));
                    }
                    Err(error) => {
                        status.publish(Err(io::Error::new(error.kind(), error.to_string())));
                        terminated.publish(Err(error));
                    }
                }
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if !observed {
            match native.observe() {
                Ok(Some(exit)) => {
                    status.publish(Ok(exit));
                    observed = true;
                }
                Ok(None) => {}
                Err(error) => {
                    // Retain the original monitoring error. Drop still attempts native
                    // cleanup, but must not signal a Unix group if its pin was lost.
                    status.publish(Err(io::Error::new(error.kind(), error.to_string())));
                    let _ = native.terminate();
                    terminated.publish(Err(error));
                    return;
                }
            }
        }
    }
}

#[derive(Clone)]
struct Registration {
    owner: Weak<Owner>,
    terminated: Arc<Completion<()>>,
}

#[derive(Clone, Default)]
struct Registry(Arc<Mutex<Vec<Registration>>>);

impl Registry {
    fn register(&self, registration: Registration) {
        let mut owners = self.0.lock().unwrap();
        // Failed completions remain pending until shutdown reports them.
        owners.retain(|entry| !entry.terminated.succeeded());
        owners.push(registration);
    }
}

/// A runtime run's cancellation guard. Dropping it requests cleanup without waiting
/// on the executor that is being destroyed. Normal completion can await shutdown.
pub struct OwnedProcessScope(Registry);

impl OwnedProcessScope {
    fn registrations(&self) -> Vec<Registration> {
        self.0.0.lock().unwrap().clone()
    }

    /// Terminates every owned tree started by the run and waits for direct reaping.
    ///
    /// # Errors
    /// Returns the first native termination or monitoring error, after all cleanups finish.
    ///
    /// # Panics
    /// Panics if the owned-process registry mutex was poisoned.
    pub async fn shutdown(&self) -> io::Result<()> {
        let registrations = self.registrations();
        for entry in &registrations {
            if let Some(owner) = entry.owner.upgrade() {
                owner.terminate();
            }
        }
        let mut result = Ok(());
        // Include handles already collected by Lua. Their drop requested cleanup,
        // but a runtime must still wait for their worker to finish reaping.
        for entry in &registrations {
            if let Err(error) = entry.terminated.wait().await {
                result = result.and(Err(error));
            }
        }
        // Retire only after every result is collected, with no intervening await.
        // A canceled shutdown must leave unreported errors for its successor.
        self.0.0.lock().unwrap().retain(|entry| {
            !registrations
                .iter()
                .any(|reported| Arc::ptr_eq(&entry.terminated, &reported.terminated))
        });
        result
    }
}

impl Drop for OwnedProcessScope {
    fn drop(&mut self) {
        for entry in self.registrations() {
            if let Some(owner) = entry.owner.upgrade() {
                owner.terminate();
            }
        }
    }
}

/// Establishes cleanup for owned processes created during a runtime run.
#[must_use]
pub fn owned_process_scope(lua: &Lua) -> OwnedProcessScope {
    let registry = lua
        .app_data_ref::<Registry>()
        .map(|registry| registry.clone())
        .unwrap_or_default();
    lua.set_app_data(registry.clone());
    OwnedProcessScope(registry)
}

#[cfg(unix)]
#[derive(Debug)]
struct Native {
    child: std::process::Child,
    group: Group,
    child_lost: bool,
    cleanup_failed: bool,
}

#[cfg(unix)]
impl Native {
    fn spawn(mut command: Command) -> io::Result<Self> {
        use std::os::unix::process::CommandExt;
        let group = Group::spawn()?;
        // The supervisor is already the leader. Assignment runs before exec, so no
        // descendant can race group assignment, even if the launcher exits at once.
        command.process_group(group.pid);
        Ok(Self {
            child: command.spawn().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Could not join owned group {}. {error}", group.pid),
                )
            })?,
            group,
            child_lost: false,
            cleanup_failed: false,
        })
    }

    fn streams(&mut self) -> (ChildWriter, ChildReader, ChildReader) {
        (
            ChildWriter::owned(self.child.stdin.take().expect("piped stdin")),
            self.child.stdout.take().expect("piped stdout").into(),
            self.child.stderr.take().expect("piped stderr").into(),
        )
    }

    fn observe(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.child_lost {
            return Err(io::Error::from_raw_os_error(libc::ECHILD));
        }
        let result = self.child.try_wait();
        if result
            .as_ref()
            .is_err_and(|error| error.raw_os_error() == Some(libc::ECHILD))
        {
            self.child_lost = true;
        }
        result
    }

    fn kill_direct(&mut self) -> io::Result<()> {
        if self.observe()?.is_none() {
            self.child.kill()?;
        }
        Ok(())
    }

    fn terminate(&mut self) -> io::Result<ExitStatus> {
        let result = self.group.terminate().and_then(|()| {
            if let Some(status) = self.observe()? {
                return Ok(status);
            }
            // The known direct child may have moved itself out of the group. Its
            // unreaped identity is still ours, so kill it too before waiting. This
            // does not extend group ownership to deliberately detached descendants.
            self.child.kill()?;
            let result = self.child.wait();
            if result
                .as_ref()
                .is_err_and(|error| error.raw_os_error() == Some(libc::ECHILD))
            {
                self.child_lost = true;
            }
            result
        });
        self.cleanup_failed = result.is_err();
        result
    }
}

/// Keeps a live, minimal leader in the group. Retaining only a zombie works on Linux,
/// but XNU's killpg1 filters zombies and returns EPERM for an otherwise empty group.
/// This process also kills its group on owner death (EOF on its private control pipe).
#[cfg(unix)]
#[derive(Debug)]
struct Group {
    pid: libc::pid_t,
    state: GroupState,
    control: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupState {
    Pinned,
    Reaped,
    Lost,
}

#[cfg(unix)]
impl Group {
    fn spawn() -> io::Result<Self> {
        Self::spawn_with_closer(close_inherited_fds)
    }

    fn spawn_with_closer(close: unsafe fn(libc::c_int) -> bool) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        let (reader, writer) = UnixStream::pair()?;
        let control = reader.as_raw_fd();
        let pid = fork_supervisor(control, close)?;
        drop(reader);
        let mut group = Self {
            pid,
            state: GroupState::Pinned,
            control: writer,
        };
        // Only the supervisor establishes its group. Readiness below acknowledges
        // both setpgid and descriptor closure before any user command can be spawned.
        // Do not launch user code until the supervisor has closed inherited FDs.
        // This also ensures abrupt owner death can be handled immediately, without
        // first walking a potentially large descriptor table.
        if let Err(error) = std::io::Read::read_exact(&mut group.control, &mut [0_u8]) {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            loop {
                let waited = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid as libc::id_t,
                        &raw mut info,
                        libc::WEXITED | libc::WNOWAIT,
                    )
                };
                if waited == 0 {
                    break;
                }
                let wait_error = io::Error::last_os_error();
                if wait_error.raw_os_error() == Some(libc::ECHILD) {
                    group.state = GroupState::Lost;
                }
                if wait_error.kind() != io::ErrorKind::Interrupted {
                    break;
                }
            }
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "Owned process supervisor {pid} failed before readiness (wait code {}, status {}). {error}",
                    info.si_code,
                    unsafe { info.si_status() },
                ),
            ));
        }
        Ok(group)
    }

    fn terminate(&mut self) -> io::Result<()> {
        match self.state {
            GroupState::Reaped => return Ok(()),
            GroupState::Lost => return Err(io::Error::from_raw_os_error(libc::ECHILD)),
            GroupState::Pinned => {}
        }
        // Verify that our leader is still waitable without consuming its identity.
        // An embedding host that reaps arbitrary children must fail closed here.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &raw mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                // Remember lost ownership permanently. A later child may reuse this
                // numeric PID, so probing it again would not establish identity.
                self.state = GroupState::Lost;
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        // The leader has not been reaped. Never signal again after reap succeeds.
        if unsafe { libc::kill(-self.pid, libc::SIGKILL) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                // Release the helper even on a native signal error. It attempts its
                // own final group kill on EOF, then exits. Preserve the original error.
                let _ = self.control.shutdown(std::net::Shutdown::Both);
                let _ = self.reap();
                return Err(error);
            }
        }
        self.reap()
    }

    fn reap(&mut self) -> io::Result<()> {
        loop {
            let result = unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), 0) };
            if result == self.pid {
                self.state = GroupState::Reaped;
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                // Remember lost ownership permanently. A later child may reuse this
                // numeric PID, so probing it again would not establish identity.
                self.state = GroupState::Lost;
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(unix)]
fn fork_supervisor(
    control: libc::c_int,
    close: unsafe fn(libc::c_int) -> bool,
) -> io::Result<libc::pid_t> {
    // Block before fork, not afterwards. Otherwise a signal could run an inherited
    // application handler in the child in the interval before it blocks signals.
    // SAFETY: valid sigset_t storage and pthread_sigmask only changes this thread.
    let mut signals: libc::sigset_t = unsafe { std::mem::zeroed() };
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigfillset(&raw mut signals);
    }
    let error =
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const signals, &raw mut previous) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    // SAFETY: the child calls only async-signal-safe C operations and _exit, never
    // Rust destructors, allocation, logging, synchronization, or inherited handlers.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe { supervisor(control, close) }
    }
    let spawn_error = (pid == -1).then(io::Error::last_os_error);
    let error = unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, &raw const previous, std::ptr::null_mut())
    };
    if let Some(error) = spawn_error {
        return Err(error);
    }
    if error != 0 {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        while unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) } == -1 {
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(pid)
}

// These native calls do not consult RLIMIT_NOFILE and do not allocate or take
// user-space locks after fork. Failure prevents the readiness acknowledgment.
#[cfg(target_os = "linux")]
unsafe fn close_inherited_fds(control: libc::c_int) -> bool {
    unsafe {
        let lower = control == 0
            || libc::syscall(libc::SYS_close_range, 0_u32, control as u32 - 1, 0_u32) == 0;
        let upper = libc::syscall(libc::SYS_close_range, control as u32 + 1, u32::MAX, 0_u32) == 0;
        if lower && upper {
            return true;
        }
        close_procfs_fds(control)
    }
}

#[cfg(target_os = "linux")]
unsafe fn close_procfs_fds(control: libc::c_int) -> bool {
    unsafe {
        // Older kernels can enumerate their actual descriptor table through procfs.
        let directory = libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        );
        if directory < 0 {
            return false;
        }
        let mut buffer = [0_u8; 8192];
        loop {
            let bytes = libc::syscall(
                libc::SYS_getdents64,
                directory,
                buffer.as_mut_ptr(),
                buffer.len(),
            );
            if bytes < 0 {
                if errno::errno().0 == libc::EINTR {
                    continue;
                }
                libc::close(directory);
                return false;
            }
            if bytes == 0 {
                libc::close(directory);
                return true;
            }
            let mut offset = 0;
            while offset < bytes as usize {
                let length = usize::from(u16::from_ne_bytes([
                    buffer[offset + 16],
                    buffer[offset + 17],
                ]));
                let mut fd = 0_i32;
                let mut numeric = false;
                for &byte in &buffer[offset + 19..offset + length] {
                    if byte == 0 {
                        break;
                    }
                    if !byte.is_ascii_digit() {
                        numeric = false;
                        break;
                    }
                    numeric = true;
                    fd *= 10;
                    fd += i32::from(byte - b'0');
                }
                if numeric && fd != control && fd != directory {
                    libc::close(fd);
                }
                offset += length;
            }
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn close_inherited_fds(control: libc::c_int) -> bool {
    unsafe {
        let mut descriptors: [libc::proc_fdinfo; 64] = std::mem::zeroed();
        loop {
            // proc_pidinfo is a thin wrapper around the proc_info system call.
            // Repeated bounded batches cover descriptors above lowered soft limits.
            let bytes = libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                descriptors.as_mut_ptr().cast(),
                std::mem::size_of_val(&descriptors) as libc::c_int,
            );
            if bytes <= 0 {
                return false;
            }
            let count = bytes as usize / std::mem::size_of::<libc::proc_fdinfo>();
            for descriptor in &descriptors[..count] {
                if descriptor.proc_fd != control {
                    libc::close(descriptor.proc_fd);
                }
            }
            if count < descriptors.len() {
                return true;
            }
        }
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
unsafe fn close_inherited_fds(control: libc::c_int) -> bool {
    #[cfg(any(
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    unsafe {
        for fd in 0..control {
            libc::close(fd);
        }
        libc::closefrom(control + 1);
        true
    }
    #[cfg(not(any(
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = control;
        false
    }
}

#[cfg(unix)]
unsafe fn supervisor(control: libc::c_int, close: unsafe fn(libc::c_int) -> bool) -> ! {
    unsafe {
        if libc::setpgid(0, 0) == -1 {
            libc::_exit(1);
        }
        if !close(control) {
            libc::_exit(3);
        }
        let mut byte = 0_u8;
        // Notify the parent that inherited descriptors have all been closed.
        loop {
            if libc::write(control, (&raw const byte).cast(), 1) == 1 {
                break;
            }
            // errno() is just the platform C errno accessor, with no allocation or
            // locks. Retry interruptions without accidentally abandoning ownership.
            if errno::errno().0 != libc::EINTR {
                libc::_exit(2);
            }
        }
        // The parent never writes to this socket and all signals are blocked.
        // EOF (or a native read error) requests cleanup, including on owner death.
        loop {
            let result = libc::read(control, (&raw mut byte).cast(), 1);
            if result == 0 || (result < 0 && errno::errno().0 != libc::EINTR) {
                break;
            }
        }
        libc::kill(-libc::getpid(), libc::SIGKILL);
        libc::_exit(1);
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct Native {
    child: Box<dyn process_wrap::std::ChildWrapper>,
}

#[cfg(windows)]
impl Native {
    fn spawn(command: Command) -> io::Result<Self> {
        use process_wrap::std::{CommandWrap, JobObject, KillOnDrop};
        // JobObject detects KillOnDrop, creates a kill-on-close job, spawns suspended,
        // assigns the child, then resumes it. Keep the wrapper/job alive after exit.
        let child = CommandWrap::from(command)
            .wrap(JobObject)
            .wrap(KillOnDrop)
            .spawn()?;
        Ok(Self { child })
    }

    fn streams(&mut self) -> (ChildWriter, ChildReader, ChildReader) {
        (
            ChildWriter::owned(self.child.stdin().take().expect("piped stdin")),
            self.child.stdout().take().expect("piped stdout").into(),
            self.child.stderr().take().expect("piped stderr").into(),
        )
    }

    fn direct(&mut self) -> &mut std::process::Child {
        // SAFETY: the chain contains only JobObject and KillOnDrop. Neither requires
        // an unreaped native child. std caches its status, so the job wrapper's later
        // wait still obtains it; the job and kill-on-close handles stay intact.
        unsafe { self.child.try_inner_child_mut().expect("native child") }
    }

    fn observe(&mut self) -> io::Result<Option<ExitStatus>> {
        // Observe just the launcher, not JobObjectChild::try_wait (the whole job).
        self.direct().try_wait()
    }

    fn kill_direct(&mut self) -> io::Result<()> {
        self.direct().kill()
    }

    fn terminate(&mut self) -> io::Result<ExitStatus> {
        self.child.start_kill()?;
        // This reaps the direct child and also waits for the job to become empty.
        self.child.wait()
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            if !self.cleanup_failed {
                let _ = self.terminate();
            }
            // On ownership failure, never fall back to signaling a raw direct PID.
            // An external reaper may have released it. Reap only if already exited.
            let _ = self.observe();
        }
        #[cfg(windows)]
        {
            // Windows retains a native process handle, so a direct fallback cannot
            // target a reused PID if job termination fails.
            if self.terminate().is_err() && self.kill_direct().is_ok() {
                let _ = self.direct().wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const DETACHED_ENV: &str = "LUNE_OWNED_PROCESS_DETACHED_FIXTURE";
    #[cfg(unix)]
    const DETACHED_READY: &str = "Owned direct child is detached.";

    #[cfg(unix)]
    #[test]
    fn owned_detached_root_fixture() {
        if std::env::var_os(DETACHED_ENV).is_none() {
            return;
        }
        assert_ne!(unsafe { libc::setsid() }, -1);
        println!("{DETACHED_READY}");
        // A missing direct-child kill must fail the timing assertion, not hang.
        thread::sleep(Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn termination_also_reaps_a_detached_direct_child() {
        use std::io::BufRead;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "create::owned::tests::owned_detached_root_fixture",
                "--nocapture",
            ])
            .env(DETACHED_ENV, "1")
            .stdout(Stdio::piped());
        let mut native = Native::spawn(command).unwrap();
        let stdout = native.child.stdout.take().unwrap();
        assert!(
            std::io::BufReader::new(stdout)
                .lines()
                .any(|line| line.unwrap().contains(DETACHED_READY))
        );
        let started = std::time::Instant::now();
        assert!(!native.terminate().unwrap().success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn registration_keeps_failures_until_shutdown_reports_them() {
        futures_lite::future::block_on(async {
            let registry = Registry::default();
            let failed = Arc::new(Completion::new());
            failed.publish(Err(io::Error::other("unreported cleanup failure")));
            let successful = Arc::new(Completion::new());
            successful.publish(Ok(()));
            let pending = Arc::new(Completion::new());
            for terminated in [&failed, &successful, &pending] {
                registry.register(Registration {
                    owner: Weak::new(),
                    terminated: Arc::clone(terminated),
                });
            }
            assert_eq!(registry.0.lock().unwrap().len(), 2);
            let scope = OwnedProcessScope(registry.clone());
            {
                let shutdown = scope.shutdown();
                futures_lite::pin!(shutdown);
                assert!(
                    futures_lite::future::poll_once(shutdown.as_mut())
                        .await
                        .is_none()
                );
            }
            // Cancellation after collecting the first error must not consume it.
            assert_eq!(registry.0.lock().unwrap().len(), 2);
            pending.publish(Ok(()));
            assert_eq!(
                scope.shutdown().await.unwrap_err().to_string(),
                "unreported cleanup failure"
            );
            assert!(registry.0.lock().unwrap().is_empty());
            assert!(scope.shutdown().await.is_ok());
            // Explicit handle-style completion reads still retain the original error.
            assert!(failed.wait().await.is_err());
        });
    }

    #[test]
    fn completion_broadcasts_and_caches_errors() {
        futures_lite::future::block_on(async {
            let completion = Completion::<()>::new();
            completion.publish(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "controlled error",
            )));
            completion.publish(Ok(()));
            for _ in 0..3 {
                let error = completion.wait().await.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(error.to_string(), "controlled error");
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn owned_lowered_descriptor_limit_fixture() {
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::net::UnixStream;
        if std::env::var_os("LUNE_OWNED_LOWERED_LIMIT_FIXTURE").is_none() {
            return;
        }
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(5));
            std::process::exit(79);
        });
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let descriptor = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 128) };
        assert!(descriptor >= 128);
        let inherited = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        limit.rlim_cur = 64;
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) },
            0
        );
        #[cfg(target_os = "linux")]
        let group = if std::env::var_os("LUNE_OWNED_PROCFS_FIXTURE").is_some() {
            Group::spawn_with_closer(close_procfs_fds).unwrap()
        } else {
            Group::spawn().unwrap()
        };
        #[cfg(not(target_os = "linux"))]
        let group = Group::spawn().unwrap();
        drop(writer);
        drop(inherited);
        let result = reader.read(&mut [0_u8]);
        drop(group);
        assert_eq!(
            result.unwrap(),
            0,
            "the supervisor retained a high pipe writer"
        );
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_closes_descriptors_above_a_lowered_soft_limit() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "create::owned::tests::owned_lowered_descriptor_limit_fixture",
                "--nocapture",
            ])
            .env("LUNE_OWNED_LOWERED_LIMIT_FIXTURE", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn procfs_fallback_closes_descriptors_above_a_lowered_soft_limit() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "create::owned::tests::owned_lowered_descriptor_limit_fixture",
                "--nocapture",
            ])
            .env("LUNE_OWNED_LOWERED_LIMIT_FIXTURE", "1")
            .env("LUNE_OWNED_PROCFS_FIXTURE", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn owned_supervisor_startup_can_overlap_previous_cleanup() {
        let mut completions = Vec::new();
        for _ in 0..40 {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "exit 0"]);
            let (owned, _stdin, _stdout, _stderr) = OwnedProcess::spawn(command).unwrap();
            completions.push(Arc::clone(&owned.0.terminated));
            drop(owned);
        }
        futures_lite::future::block_on(async {
            for completion in completions {
                completion.wait().await.unwrap();
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn owned_group_stays_pinned_after_direct_child_reaping() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 23"]);
        let mut native = Native::spawn(command).unwrap();
        let pid = native.child.id();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = native.observe().unwrap() {
                assert_eq!(status.code(), Some(23));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        // The direct child can be reaped without releasing the group leader.
        assert_eq!(native.observe().unwrap().unwrap().code(), Some(23));
        assert_eq!(native.group.state, GroupState::Pinned);
        assert_eq!(native.terminate().unwrap().code(), Some(23));
        assert_eq!(native.group.state, GroupState::Reaped);
        assert_eq!(native.terminate().unwrap().code(), Some(23));
        // SAFETY: WNOHANG and the exact PID cannot block or reap another child.
        let result =
            unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(result, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[cfg(unix)]
    #[test]
    fn monitor_error_is_cached_and_still_cleans_up_the_group() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let mut native = Native::spawn(command).unwrap();
        // Reap outside std::Child, leaving its internal status cache unset.
        let pid = native.child.id() as libc::pid_t;
        assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) }, pid);
        let (_tx, rx) = mpsc::channel();
        let status = Completion::new();
        let terminated = Completion::new();
        supervise(&mut native, &rx, &status, &terminated);
        assert_eq!(native.group.state, GroupState::Reaped);
        assert!(native.child_lost);
        assert_eq!(
            native.observe().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert_eq!(
            native.kill_direct().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        futures_lite::future::block_on(async {
            for _ in 0..2 {
                assert!(status.wait().await.is_err());
                assert!(terminated.wait().await.is_err());
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn lost_identity_pin_fails_closed_and_reports_termination_error() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let mut native = Native::spawn(command).unwrap();
        // Simulate an embedding host reaping the identity pin. No group signal may
        // follow, even if a user command has already been reaped successfully.
        native.child.wait().unwrap();
        unsafe {
            libc::kill(native.group.pid, libc::SIGKILL);
        }
        let pid = native.group.pid;
        assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) }, pid);
        assert_eq!(
            native.terminate().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        // Model numeric PID reuse with a different, live group. Once loss was
        // observed, neither another call nor Drop may probe or signal that identity.
        let unrelated = Group::spawn().unwrap();
        native.group.pid = unrelated.pid;
        assert_eq!(
            native.terminate().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    unrelated.pid as libc::id_t,
                    &raw mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(
            unsafe { info.si_pid() },
            0,
            "the unrelated group must remain alive"
        );
        let (tx, rx) = mpsc::channel();
        tx.send(Request::Terminate).unwrap();
        let status = Completion::new();
        let terminated = Completion::new();
        supervise(&mut native, &rx, &status, &terminated);
        futures_lite::future::block_on(async {
            for _ in 0..2 {
                assert_eq!(
                    status.wait().await.unwrap_err().kind(),
                    io::Error::from_raw_os_error(libc::ECHILD).kind()
                );
                assert!(terminated.wait().await.is_err());
            }
        });
    }
}
