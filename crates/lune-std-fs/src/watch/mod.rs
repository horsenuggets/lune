use std::{cell::Cell, io::ErrorKind as IoErrorKind, rc::Rc, time::Duration};

use async_channel::Receiver;
use async_io::Timer;
use futures_lite::future::or;
use mlua::prelude::*;
use mlua_luau_scheduler::{LuaSchedulerExt, LuaSpawnExt};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use lune_utils::path::clean_path_and_make_absolute;

mod event;
mod handle;
mod options;

use self::event::{WatchEvent, WatchTarget, push_unique};
use self::handle::WatchHandle;
use self::options::FsWatchOptions;

type RawEvent = notify::Result<Event>;

/**
    Starts watching the file or directory at the given path for changes.

    Returns a handle table with a `stop` function. The watcher keeps the
    process alive until it is stopped, and callbacks run on the scheduler,
    one at a time, in the order events were received.
*/
pub fn fs_watch(
    lua: &Lua,
    (path, options_or_callback, callback): (String, LuaValue, LuaValue),
) -> LuaResult<LuaTable> {
    let (options, callback) = parse_args(lua, options_or_callback, callback)?;

    let target_path = clean_path_and_make_absolute(&path);
    let metadata = match std::fs::metadata(&target_path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == IoErrorKind::NotFound => {
            return Err(LuaError::RuntimeError(format!(
                "No file or directory exists at the path '{path}'"
            )));
        }
        Err(e) => return Err(e.into()),
    };

    // NOTE: Single files are watched through their parent directory,
    // see the documentation on `WatchTarget` for why this is necessary
    let (root, file, mode) = if metadata.is_dir() {
        let mode = if options.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        (target_path, None, mode)
    } else {
        let parent = target_path
            .parent()
            .ok_or_else(|| {
                LuaError::RuntimeError(format!("Could not find the parent directory of '{path}'"))
            })?
            .to_path_buf();
        (parent, Some(target_path), RecursiveMode::NonRecursive)
    };
    let canonical_root = std::fs::canonicalize(&root).ok();

    let (event_tx, event_rx) = async_channel::unbounded::<RawEvent>();
    let mut watcher = notify::recommended_watcher(move |res: RawEvent| {
        // NOTE: This runs on a thread owned by notify, so we only forward
        // events here, and never touch Luau until we are back on the scheduler
        event_tx.try_send(res).ok();
    })
    .map_err(|e| LuaError::RuntimeError(format!("Failed to watch the path '{path}' - {e}")))?;
    watcher
        .watch(&root, mode)
        .map_err(|e| LuaError::RuntimeError(format!("Failed to watch the path '{path}' - {e}")))?;

    let (handle, shutdown_rx, stopped) = WatchHandle::new();
    let source = EventSource {
        events: event_rx,
        handle_dropped: false,
        shutdown: shutdown_rx,
        stopped: Rc::clone(&stopped),
        target: WatchTarget::new(root, canonical_root, file),
    };
    lua.spawn_local(run_watcher(
        lua.clone(),
        watcher,
        source,
        options.debounce,
        callback,
        stopped,
    ));

    handle.into_lua_table(lua.clone())
}

/**
    Parses the trailing arguments of `fs.watch`, which can be either
    `(callback)` or `(options, callback)`, where options may be `nil`.
*/
fn parse_args(
    lua: &Lua,
    options_or_callback: LuaValue,
    callback: LuaValue,
) -> LuaResult<(FsWatchOptions, LuaFunction)> {
    match (options_or_callback, callback) {
        (LuaValue::Function(callback), LuaValue::Nil) => Ok((FsWatchOptions::default(), callback)),
        (options, LuaValue::Function(callback)) => {
            Ok((FsWatchOptions::from_lua(options, lua)?, callback))
        }
        (_, callback) => Err(LuaError::RuntimeError(format!(
            "Invalid watch callback - expected function, got {}",
            callback.type_name()
        ))),
    }
}

enum Received {
    Closed,
    Event(RawEvent),
    HandleDropped,
    Stopped,
}

/**
    Yields translated watch events until the watcher gets stopped.
*/
struct EventSource {
    events: Receiver<RawEvent>,
    handle_dropped: bool,
    shutdown: Receiver<()>,
    stopped: Rc<Cell<bool>>,
    target: WatchTarget,
}

impl EventSource {
    async fn next(&mut self) -> Option<WatchEvent> {
        loop {
            let received = if self.handle_dropped {
                match self.events.recv().await {
                    Ok(res) => Received::Event(res),
                    Err(_) => Received::Closed,
                }
            } else {
                let (shutdown_rx, events_rx) = (&self.shutdown, &self.events);
                let shutdown = async {
                    match shutdown_rx.recv().await {
                        Ok(()) => Received::Stopped,
                        Err(_) => Received::HandleDropped,
                    }
                };
                let events = async {
                    match events_rx.recv().await {
                        Ok(res) => Received::Event(res),
                        Err(_) => Received::Closed,
                    }
                };
                or(shutdown, events).await
            };
            match received {
                Received::Stopped | Received::Closed => return None,
                Received::HandleDropped => {
                    if self.stopped.get() {
                        return None;
                    }
                    // The handle was garbage collected without being stopped,
                    // meaning the user wants to keep watching forever
                    self.handle_dropped = true;
                }
                Received::Event(Ok(event)) => {
                    if let Some(event) = self.target.translate(event.kind, &event.paths) {
                        return Some(event);
                    }
                }
                Received::Event(Err(_)) => {
                    // TODO: Propagate watcher errors somehow
                }
            }
        }
    }
}

async fn run_watcher(
    lua: Lua,
    watcher: RecommendedWatcher,
    mut source: EventSource,
    debounce: Duration,
    callback: LuaFunction,
    stopped: Rc<Cell<bool>>,
) {
    // Keep the OS-level watcher alive for as long as this task runs,
    // it gets dropped (and stops watching) once the task completes
    let _watcher = watcher;

    while let Some(first) = source.next().await {
        let mut batch = vec![first];

        // Collect events until there has been no new event for
        // the full debounce duration, dropping exact duplicates
        if !debounce.is_zero() {
            loop {
                let next = async { Some(source.next().await) };
                let quiet = async {
                    Timer::after(debounce).await;
                    None
                };
                match or(next, quiet).await {
                    Some(Some(event)) => push_unique(&mut batch, event),
                    Some(None) => return,
                    None => break,
                }
            }
        }

        for event in batch {
            if stopped.get() {
                return;
            }
            deliver(&lua, &callback, event).await.ok();
        }
    }
}

/**
    Runs the callback for a single event on the scheduler, and waits for it
    to finish, so that callbacks never overlap even if they yield.

    Errors thrown inside the callback are reported by the scheduler.
*/
async fn deliver(lua: &Lua, callback: &LuaFunction, event: WatchEvent) -> LuaResult<()> {
    let thread_id = lua.push_thread_back(callback.clone(), event)?;
    lua.track_thread(thread_id);
    lua.wait_for_thread(thread_id).await;
    lua.get_thread_result(thread_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_every_valid_shape() {
        let lua = Lua::new();
        let callback = LuaValue::Function(lua.create_function(|_, ()| Ok(())).unwrap());
        let options = LuaValue::Table(lua.load("return { recursive = false }").eval().unwrap());

        let (parsed, _) = parse_args(&lua, callback.clone(), LuaValue::Nil).unwrap();
        assert_eq!(parsed, FsWatchOptions::default());

        let (parsed, _) = parse_args(&lua, LuaValue::Nil, callback.clone()).unwrap();
        assert_eq!(parsed, FsWatchOptions::default());

        let (parsed, _) = parse_args(&lua, options, callback).unwrap();
        assert!(!parsed.recursive);
    }

    #[test]
    fn parse_args_rejects_missing_or_invalid_callbacks() {
        let lua = Lua::new();
        let options = LuaValue::Table(lua.create_table().unwrap());
        assert!(parse_args(&lua, LuaValue::Nil, LuaValue::Nil).is_err());
        assert!(parse_args(&lua, options, LuaValue::Boolean(true)).is_err());
        let callback = LuaValue::Function(lua.create_function(|_, ()| Ok(())).unwrap());
        assert!(parse_args(&lua, LuaValue::Boolean(true), callback).is_err());
    }
}
