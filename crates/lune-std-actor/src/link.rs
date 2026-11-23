use std::{
    any::Any,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, PoisonError,
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
        Decides the outcome of an actor whose runtime finished running its script.

        Termination takes priority, then explicit exit codes, then errors from any of the
        actor's threads. Otherwise the script's returned values are copied for the owner.
    */
    #[must_use]
    pub fn from_run(
        link: &ActorLink,
        code: Option<u8>,
        first_error: Option<String>,
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
        if let Some(error) = first_error {
            return Self::Failed(error);
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
}

impl ActorLink {
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
        }
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

    /// Whether the actor's OS thread may still be running.
    pub(crate) fn has_running_thread(&self) -> bool {
        self.thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }
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
    the owner is never left waiting for an actor that crashed.

    # Errors

    Errors if the operating system cannot create the thread.
*/
pub fn spawn_actor_thread<F>(link: Arc<ActorLink>, body: F) -> io::Result<()>
where
    F: FnOnce(Arc<ActorLink>) -> ActorOutcome + Send + 'static,
{
    let thread_link = Arc::clone(&link);
    let handle = Builder::new()
        .name("lune-actor".to_string())
        .spawn(move || {
            let body_link = Arc::clone(&thread_link);
            let outcome =
                catch_unwind(AssertUnwindSafe(move || body(body_link))).unwrap_or_else(|payload| {
                    ActorOutcome::Failed(format!(
                        "The actor panicked with {}.",
                        panic_message(payload.as_ref())
                    ))
                });
            thread_link.finish(outcome);
        })?;
    *link.thread.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Returned values are exact integers, so exact comparisons are intended
mod tests {
    use async_io::block_on;

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
        assert!(!link.has_running_thread());
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
    fn outcome_priorities() {
        let lua = Lua::new();
        let values = LuaMultiValue::from_vec(vec![LuaValue::Number(1.0)]);
        let fresh = link();
        assert!(matches!(
            ActorOutcome::from_run(&fresh, None, None, &values),
            ActorOutcome::Completed(_)
        ));
        assert!(matches!(
            ActorOutcome::from_run(&fresh, Some(0), None, &values),
            ActorOutcome::Completed(v) if v.is_empty()
        ));
        assert!(matches!(
            ActorOutcome::from_run(&fresh, Some(3), Some("error".into()), &values),
            ActorOutcome::Failed(m) if m.contains("code 3")
        ));
        assert!(matches!(
            ActorOutcome::from_run(&fresh, None, Some("first".into()), &values),
            ActorOutcome::Failed(m) if m == "first"
        ));
        let function = LuaMultiValue::from_vec(vec![LuaValue::Function(
            lua.create_function(|_, ()| Ok(())).unwrap(),
        )]);
        assert!(matches!(
            ActorOutcome::from_run(&fresh, None, None, &function),
            ActorOutcome::Failed(m) if m.contains("cannot be sent")
        ));
        let terminated = link();
        terminated.terminate();
        assert!(matches!(
            ActorOutcome::from_run(&terminated, Some(0), None, &values),
            ActorOutcome::Failed(m) if m == TERMINATED_MESSAGE
        ));
    }
}
