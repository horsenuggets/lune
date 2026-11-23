use std::{
    any::Any,
    cell::Cell,
    io,
    panic::{self, AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, Once, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::{Builder, JoinHandle},
};

use async_channel::{Receiver, Sender};
use mlua::prelude::*;
use mlua_luau_scheduler::ExitHandle;

use crate::{mailbox::Mailbox, transfer::Transfer};

/// The error an actor reports after its owner terminated it.
pub const TERMINATED_MESSAGE: &str = "The actor was terminated.";

/// The exit code an actor's scheduler stops with when it is terminated.
const TERMINATED_EXIT_CODE: u8 = 1;

/**
    The final result of an actor, delivered to its owner through `join`.
*/
#[derive(Debug, Clone)]
pub enum ActorOutcome {
    /// The actor finished, returning these values from its script.
    Completed(Vec<Transfer>),
    /// The actor failed, with an error message that includes any traceback.
    Failed(String),
}

impl ActorOutcome {
    /**
        Decides the outcome of an actor whose runtime could not be set up or could not
        start its script. Termination takes priority, exactly as in [`Self::from_run`],
        since terminating an actor during setup also makes the setup fail.
    */
    #[must_use]
    pub fn from_failure(link: &ActorLink, message: String) -> Self {
        if link.is_terminated() {
            Self::Failed(TERMINATED_MESSAGE.to_string())
        } else {
            Self::Failed(message)
        }
    }

    /**
        Decides the outcome of an actor whose runtime finished running its script.

        Termination takes priority, then explicit exit codes, then errors from any of the
        actor's threads, joined in the order they happened. A script that ended without
        returning or erroring also fails, so an actor can never silently pass. Otherwise
        the script's returned values are copied for the owner.
    */
    #[must_use]
    pub fn from_run(
        link: &ActorLink,
        code: Option<u8>,
        errors: &[String],
        finished: bool,
        values: &LuaMultiValue,
    ) -> Self {
        if link.is_terminated() {
            return Self::Failed(TERMINATED_MESSAGE.to_string());
        }
        match code {
            Some(0) => return Self::Completed(Vec::new()),
            Some(code) => return Self::Failed(format!("The actor exited with code {code}.")),
            None => {}
        }
        if !errors.is_empty() {
            return Self::Failed(errors.join("\n\n"));
        }
        if !finished {
            return Self::Failed(
                "The actor's script ended without returning a result or an error.".to_string(),
            );
        }
        match Transfer::from_lua_multi(values) {
            Ok(values) => Self::Completed(values),
            Err(error) => Self::Failed(format!(
                "The actor returned a value that cannot be sent to its owner. {error}"
            )),
        }
    }
}

/**
    The state shared between an actor's OS thread and the VM that owns it.
*/
#[derive(Debug)]
pub struct ActorLink {
    pub(crate) args: Transfer,
    pub(crate) inbox: Mailbox,
    pub(crate) outbox: Mailbox,
    terminated: AtomicBool,
    exit: Mutex<Option<ExitHandle>>,
    outcome: Mutex<Option<ActorOutcome>>,
    done_rx: Receiver<()>,
    done_tx: Sender<()>,
    thread: Mutex<Option<JoinHandle<()>>>,
    alive: AtomicBool,
}

impl ActorLink {
    /**
        Creates a link with no arguments, optionally already terminated.

        # WARNING

        This is an API meant only for private consumption by the main `lune`
        crate's tests, which need a link to check how a runtime reports
        termination. It is NOT guaranteed to follow semver or remain stable.
    */
    #[doc(hidden)]
    #[must_use]
    pub fn for_testing(terminated: bool) -> Arc<Self> {
        let link = Arc::new(Self::new(Transfer::Nil));
        if terminated {
            link.terminate();
        }
        link
    }

    pub(crate) fn new(args: Transfer) -> Self {
        let (done_tx, done_rx) = async_channel::bounded(1);
        Self {
            args,
            inbox: Mailbox::default(),
            outbox: Mailbox::default(),
            terminated: AtomicBool::new(false),
            exit: Mutex::new(None),
            outcome: Mutex::new(None),
            done_rx,
            done_tx,
            thread: Mutex::new(None),
            alive: AtomicBool::new(false),
        }
    }

    /**
        Whether the actor's OS thread is still running its body. The thread clears this
        as the very last thing it does, so joining the thread always observes `false`.
    */
    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /**
        Connects the actor's scheduler, so that terminating the actor also stops it.

        If the actor was already terminated, the scheduler stops right away.
    */
    fn attach_exit(&self, handle: ExitHandle) {
        let mut exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        if self.is_terminated() {
            handle.exit(TERMINATED_EXIT_CODE);
        }
        *exit = Some(handle);
    }

    /// Whether the actor's owner has terminated it.
    #[must_use]
    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
    }

    pub(crate) fn terminated_flag(&self) -> *const AtomicBool {
        &raw const self.terminated
    }

    /**
        Prepares the actor's VM on the actor's own OS thread. Terminating the actor then
        stops its scheduler, even while it is idle, and makes running Luau code error at
        its next interrupt check.

        # Errors

        Errors if this OS thread already runs the VM of another actor.
    */
    pub fn attach(self: &Arc<Self>, lua: &Lua, handle: ExitHandle) -> LuaResult<()> {
        crate::interrupt::install(lua, Arc::clone(self))?;
        self.attach_exit(handle);
        Ok(())
    }

    /// Whether the actor has delivered its final outcome.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /**
        Asks the actor to stop. Its running Luau code errors at the next interrupt check,
        its scheduler stops even when idle, and it receives no further messages.
    */
    pub(crate) fn terminate(&self) {
        if self.is_finished() {
            return;
        }
        let exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        self.terminated.store(true, Ordering::SeqCst);
        self.inbox.close();
        if let Some(handle) = exit.as_ref() {
            handle.exit(TERMINATED_EXIT_CODE);
        }
    }

    fn finish(&self, outcome: ActorOutcome) {
        {
            let mut current = self.outcome.lock().unwrap_or_else(PoisonError::into_inner);
            if current.is_none() {
                *current = Some(outcome);
            }
        }
        // Owners can still receive messages that were sent before the actor finished.
        self.outbox.close();
        self.done_tx.close();
    }

    /// Waits until the actor delivers its final outcome.
    pub(crate) async fn wait(&self) -> ActorOutcome {
        loop {
            if let Some(outcome) = self
                .outcome
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
            {
                return outcome;
            }
            // The channel never carries messages. It closes once the outcome is stored.
            let _ = self.done_rx.recv().await;
        }
    }

    /// Takes the handle of the actor's OS thread, so that the caller can join it.
    pub(crate) fn take_thread(&self) -> Option<JoinHandle<()>> {
        self.thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/**
    The stack size of every actor thread. Rust's default for spawned threads is only
    2 MiB, while the main thread of Linux and macOS processes gets 8 MiB, so actors use
    the same 8 MiB to allow scripts the same recursion depth as their owner.
*/
const ACTOR_STACK_SIZE: usize = 8 * 1024 * 1024;

/// Clears an actor's liveness flag once its thread is done, even when it panics.
struct AliveGuard(Arc<ActorLink>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.alive.store(false, Ordering::SeqCst);
    }
}

thread_local! {
    static ON_ACTOR_THREAD: Cell<bool> = const { Cell::new(false) };
}

static QUIET_ACTOR_PANICS: Once = Once::new();

/**
    Keeps panics on actor threads from printing to stderr. Every actor panic is caught
    and delivered to its owner through `join`, so printing it as well would interleave
    unrelated output from parallel actors. Panics on any other thread still reach the
    panic hook that was installed before, so the main runtime prints exactly as before.
*/
fn quiet_actor_panics() {
    QUIET_ACTOR_PANICS.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !ON_ACTOR_THREAD.with(Cell::get) {
                previous(info);
            }
        }));
    });
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "an unknown panic payload".to_string()
    }
}

/**
    Runs an actor's body on a new OS thread and delivers its outcome to the link.

    Panics in the body are caught at the thread boundary and reported as failures, so
    the owner is never left waiting for an actor that crashed. They are delivered only
    through the outcome, never printed.

    # Errors

    Errors if the operating system cannot create the thread.
*/
pub fn spawn_actor_thread<F>(link: Arc<ActorLink>, body: F) -> io::Result<()>
where
    F: FnOnce(Arc<ActorLink>) -> ActorOutcome + Send + 'static,
{
    quiet_actor_panics();
    let thread_link = Arc::clone(&link);
    link.alive.store(true, Ordering::SeqCst);
    let spawned = Builder::new()
        .name("lune-actor".to_string())
        .stack_size(ACTOR_STACK_SIZE)
        .spawn(move || {
            ON_ACTOR_THREAD.with(|on_actor| on_actor.set(true));
            let _alive = AliveGuard(Arc::clone(&thread_link));
            let body_link = Arc::clone(&thread_link);
            let outcome =
                catch_unwind(AssertUnwindSafe(move || body(body_link))).unwrap_or_else(|payload| {
                    ActorOutcome::Failed(format!(
                        "The actor panicked with {}.",
                        panic_message(payload.as_ref())
                    ))
                });
            thread_link.finish(outcome);
        });
    let handle = match spawned {
        Ok(handle) => handle,
        Err(error) => {
            link.alive.store(false, Ordering::SeqCst);
            return Err(error);
        }
    };
    *link.thread.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Returned values are exact integers, so exact comparisons are intended
mod tests {
    use async_io::block_on;
    use mlua_luau_scheduler::LuaSpawnExt;

    use super::*;

    fn link() -> Arc<ActorLink> {
        Arc::new(ActorLink::new(Transfer::Nil))
    }

    #[test]
    fn reports_panics_as_failures() {
        let link = link();
        spawn_actor_thread(Arc::clone(&link), |_| panic!("controlled panic")).unwrap();
        match block_on(link.wait()) {
            ActorOutcome::Failed(message) => {
                assert!(message.contains("panicked"), "{message}");
                assert!(message.contains("controlled panic"), "{message}");
            }
            ActorOutcome::Completed(_) => panic!("a panic must fail the actor"),
        }
        link.take_thread().unwrap().join().unwrap();
    }

    /**
        Runs a script on an actor thread through a real scheduler, the way a runtime does,
        with `install` adding the Rust functions the script calls, and returns the outcome.
    */
    fn run_actor_script(script: &'static str, install: fn(&Lua) -> LuaResult<()>) -> ActorOutcome {
        let link = link();
        spawn_actor_thread(Arc::clone(&link), move |link| {
            let lua = Lua::new();
            let scheduler = mlua_luau_scheduler::Scheduler::new(lua.clone());
            link.attach(&lua, scheduler.exit_handle()).unwrap();
            let errors = Arc::new(Mutex::new(Vec::new()));
            let errors_inner = Arc::clone(&errors);
            scheduler.set_error_callback(move |error| {
                errors_inner.lock().unwrap().push(error.to_string());
            });
            install(&lua).unwrap();
            let id = scheduler.push_thread_back(lua.load(script), ()).unwrap();
            block_on(scheduler.run());
            let result = scheduler.get_thread_result(id);
            let finished = result.is_some();
            let values = result.and_then(Result::ok).unwrap_or_default();
            let errors = errors.lock().unwrap().clone();
            ActorOutcome::from_run(&link, scheduler.get_exit_code(), &errors, finished, &values)
        })
        .unwrap();
        let outcome = block_on(link.wait());
        link.take_thread().unwrap().join().unwrap();
        outcome
    }

    const PANIC_OUTPUT_CHILD: &str = "LUNE_ACTOR_PANIC_OUTPUT_CHILD";

    #[test]
    fn actor_panics_reach_join_without_printing() {
        // Panic output goes to the process's stderr, so the scenario runs in a child copy
        // of this test binary and the parent inspects what the child printed.
        if std::env::var_os(PANIC_OUTPUT_CHILD).is_some() {
            let link = link();
            spawn_actor_thread(Arc::clone(&link), |_| panic!("hidden actor panic")).unwrap();
            let outcome = block_on(link.wait());
            link.take_thread().unwrap().join().unwrap();
            assert!(
                matches!(&outcome, ActorOutcome::Failed(message) if message.contains("hidden actor panic")),
                "the actor panic must still reach join: {outcome:?}"
            );
            let _ = std::thread::spawn(|| panic!("visible thread panic")).join();
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "link::tests::actor_panics_reach_join_without_printing",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PANIC_OUTPUT_CHILD, "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "the child scenario failed: {stderr}"
        );
        assert!(
            !stderr.contains("hidden actor panic"),
            "an actor panic must not print: {stderr}"
        );
        assert!(
            stderr.contains("visible thread panic"),
            "other threads must keep printing panics: {stderr}"
        );
    }

    #[test]
    fn reports_panics_in_luau_called_rust_as_failures() {
        let outcome = run_actor_script("explode() return 1", |lua| {
            lua.globals().set(
                "explode",
                lua.create_function(|_, ()| -> LuaResult<()> { panic!("controlled panic") })?,
            )
        });
        match outcome {
            ActorOutcome::Failed(message) => {
                assert!(message.contains("controlled panic"), "{message}");
            }
            ActorOutcome::Completed(_) => panic!("a panic in Luau-called Rust must fail"),
        }
    }

    #[test]
    fn reports_panics_of_detached_tasks_as_failures() {
        // The script's own thread keeps running and returns, while a background task it
        // started panics. The actor must still fail instead of joining as a success.
        let outcome = run_actor_script("startDetached() pause() return 1", |lua| {
            lua.globals().set(
                "startDetached",
                lua.create_function(|lua, ()| {
                    lua.spawn(async { panic!("controlled detached panic") })
                        .detach();
                    Ok(())
                })?,
            )?;
            lua.globals().set(
                "pause",
                lua.create_async_function(|_, ()| async {
                    async_io::Timer::after(std::time::Duration::from_millis(50)).await;
                    Ok(())
                })?,
            )
        });
        match outcome {
            ActorOutcome::Failed(message) => {
                assert!(message.contains("controlled detached panic"), "{message}");
            }
            ActorOutcome::Completed(_) => panic!("a panic of a detached task must fail"),
        }
    }

    #[test]
    fn delivers_the_outcome_to_every_waiter() {
        let link = link();
        spawn_actor_thread(Arc::clone(&link), |_| {
            ActorOutcome::Completed(vec![Transfer::Number(4.0)])
        })
        .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                block_on(link.wait()),
                ActorOutcome::Completed(values) if matches!(values[..], [Transfer::Number(n)] if n == 4.0)
            ));
        }
        link.take_thread().unwrap().join().unwrap();
        assert!(!link.is_alive());
    }

    #[test]
    fn termination_before_attaching_stops_the_scheduler_immediately() {
        let link = link();
        link.terminate();
        let lua = Lua::new();
        let scheduler = mlua_luau_scheduler::Scheduler::new(lua.clone());
        link.attach(&lua, scheduler.exit_handle()).unwrap();
        assert_eq!(scheduler.get_exit_code(), Some(TERMINATED_EXIT_CODE));
        assert!(link.inbox.is_closed());
    }

    #[test]
    fn termination_interrupts_running_luau_code() {
        let link = link();
        let lua = Lua::new();
        let scheduler = mlua_luau_scheduler::Scheduler::new(lua.clone());
        link.attach(&lua, scheduler.exit_handle()).unwrap();
        let flag = Arc::clone(&link);
        lua.globals()
            .set(
                "terminate",
                lua.create_function(move |_, ()| {
                    flag.terminate();
                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();
        // Uninterrupted code runs normally, and the loop fails once terminated.
        let message = lua
            .load("local count = 0 while true do count += 1 if count == 1000 then terminate() end end")
            .exec()
            .unwrap_err()
            .to_string();
        assert!(message.contains(TERMINATED_MESSAGE), "{message}");
    }

    #[test]
    fn setup_failures_report_termination_first() {
        // Terminating an actor while its runtime is still being set up makes that setup
        // fail too, and the owner must still see the documented termination message.
        let fresh = link();
        assert!(matches!(
            ActorOutcome::from_failure(&fresh, "Failed to create standard library".into()),
            ActorOutcome::Failed(m) if m == "Failed to create standard library"
        ));
        let terminated = link();
        terminated.terminate();
        assert!(matches!(
            ActorOutcome::from_failure(&terminated, "Failed to create standard library".into()),
            ActorOutcome::Failed(m) if m == TERMINATED_MESSAGE
        ));
    }

    #[test]
    fn outcome_priorities() {
        let lua = Lua::new();
        let values = LuaMultiValue::from_vec(vec![LuaValue::Number(1.0)]);
        let fresh = link();
        let none: &[String] = &[];
        assert!(matches!(
            ActorOutcome::from_run(&fresh, None, none, true, &values),
            ActorOutcome::Completed(_)
        ));
        assert!(matches!(
            ActorOutcome::from_run(&fresh, Some(0), none, false, &values),
            ActorOutcome::Completed(v) if v.is_empty()
        ));
        assert!(matches!(
            ActorOutcome::from_run(&fresh, Some(3), &["error".into()], true, &values),
            ActorOutcome::Failed(m) if m.contains("code 3")
        ));
        assert!(matches!(
            ActorOutcome::from_run(&fresh, None, &["first".into(), "second".into()], true, &values),
            ActorOutcome::Failed(m) if m == "first\n\nsecond"
        ));
        // A script that vanished without a result or an error must never pass.
        assert!(matches!(
            ActorOutcome::from_run(&fresh, None, none, false, &values),
            ActorOutcome::Failed(m) if m.contains("without returning")
        ));
        let function = LuaMultiValue::from_vec(vec![LuaValue::Function(
            lua.create_function(|_, ()| Ok(())).unwrap(),
        )]);
        assert!(matches!(
            ActorOutcome::from_run(&fresh, None, none, true, &function),
            ActorOutcome::Failed(m) if m.contains("cannot be sent")
        ));
        let terminated = link();
        terminated.terminate();
        assert!(matches!(
            ActorOutcome::from_run(&terminated, Some(0), none, true, &values),
            ActorOutcome::Failed(m) if m == TERMINATED_MESSAGE
        ));
    }
}
