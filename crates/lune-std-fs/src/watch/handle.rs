use std::{cell::Cell, rc::Rc};

use async_channel::{Receiver, Sender, unbounded};

use lune_utils::TableBuilder;
use mlua::prelude::*;

/**
    A handle to an active watcher, returned from `fs.watch`.

    Mirrors the handle returned from `net.serve`, exposing a single
    `stop` function that is called using dot syntax (`watcher.stop()`).
*/
#[derive(Debug, Clone)]
pub struct WatchHandle {
    sender: Sender<()>,
    stopped: Rc<Cell<bool>>,
}

impl WatchHandle {
    /**
        Creates a new handle, along with the receiving end of its
        shutdown channel and a flag that is set once it has been stopped.

        If the handle gets garbage collected without being stopped, the
        shutdown channel closes without receiving anything, and the watcher
        keeps running for the rest of the process, just like `net.serve`.
    */
    pub fn new() -> (Self, Receiver<()>, Rc<Cell<bool>>) {
        let (sender, receiver) = unbounded();
        let stopped = Rc::new(Cell::new(false));
        let this = Self {
            sender,
            stopped: Rc::clone(&stopped),
        };
        (this, receiver, stopped)
    }

    /**
        Stops the watcher.

        # Errors

        Errors if the watcher has already been stopped.
    */
    pub fn stop(&self) -> LuaResult<()> {
        if self.stopped.get() {
            Err(LuaError::runtime("Watcher already stopped"))
        } else {
            self.stopped.set(true);
            self.sender.try_send(()).ok();
            self.sender.close();
            Ok(())
        }
    }

    pub fn into_lua_table(self, lua: Lua) -> LuaResult<LuaTable> {
        TableBuilder::new(lua)?
            .with_function("stop", move |_, ()| self.stop())?
            .build_readonly()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_signals_once_and_then_errors() {
        let (handle, receiver, stopped) = WatchHandle::new();
        assert!(!stopped.get());
        assert!(handle.stop().is_ok());
        assert!(stopped.get());
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(receiver.is_closed());
        assert!(handle.stop().is_err());
    }

    #[test]
    fn dropping_handle_closes_channel_without_stopping() {
        let (handle, receiver, stopped) = WatchHandle::new();
        drop(handle);
        assert!(receiver.is_closed());
        assert!(receiver.try_recv().is_err());
        assert!(!stopped.get());
    }
}
