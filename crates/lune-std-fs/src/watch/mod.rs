use std::{
    cell::Cell,
    ffi::{OsStr, OsString},
    io::ErrorKind as IoErrorKind,
    path::Path,
    rc::Rc,
    time::Duration,
};

use async_channel::Receiver;
use async_io::Timer;
use futures_lite::future::{self, or};
use mlua::prelude::*;
use mlua_luau_scheduler::{LuaSchedulerExt, LuaSpawnExt};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use lune_utils::path::clean_path_and_make_absolute;

mod event;
mod handle;
mod options;
mod queue;

use self::event::{WatchEvent, WatchTarget, push_debounced};
use self::handle::WatchHandle;
use self::options::FsWatchOptions;
use self::queue::{EventQueue, QUEUE_CAPACITY, event_queue};

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
        check_stored_spelling(&parent, &target_path, &path)?;
        (parent, Some(target_path), RecursiveMode::NonRecursive)
    };
    let canonical_root = std::fs::canonicalize(&root).ok();

    let target = WatchTarget::new(root.clone(), canonical_root, file);
    let (forwarder, queue) = event_queue(target, QUEUE_CAPACITY);
    let mut watcher = notify::recommended_watcher(move |res| {
        // NOTE: This runs on a thread owned by notify, so we only queue
        // events here, and never touch Luau until we are back on the scheduler
        forwarder.forward(res);
    })
    .map_err(|e| LuaError::RuntimeError(format!("Failed to watch the path '{path}' - {e}")))?;
    watcher
        .watch(&root, mode)
        .map_err(|e| LuaError::RuntimeError(format!("Failed to watch the path '{path}' - {e}")))?;

    let (handle, shutdown_rx, stopped) = WatchHandle::new();
    let source = EventSource {
        handle_dropped: false,
        queue,
        shutdown: shutdown_rx,
        stopped,
    };
    lua.spawn_local(run_watcher(
        lua.clone(),
        watcher,
        source,
        options.debounce,
        callback,
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

/**
    Makes sure a single watched file is named exactly as it's stored in its parent
    directory, and errors otherwise.

    On case-insensitive filesystems (the default on Windows and macOS), a file can be
    opened using any casing, but OS-level watchers always report the stored name, so a
    watch using another spelling would silently receive nothing. Guessing which spellings
    a filesystem treats as equal is unreliable (Unicode case folding differs between
    filesystems), so the stored name is required instead, and the error names it.
*/
fn check_stored_spelling(parent: &Path, file: &Path, path: &str) -> LuaResult<()> {
    let Some(name) = file.file_name() else {
        return Ok(());
    };
    let entries: Vec<OsString> = std::fs::read_dir(parent)?
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    match check_spelling(name, &entries) {
        Ok(()) => Ok(()),
        Err(Some(stored)) => Err(LuaError::RuntimeError(format!(
            "The file at '{path}' is stored as '{}', watch it using that name instead",
            stored.to_string_lossy()
        ))),
        Err(None) => Err(LuaError::RuntimeError(format!(
            "The file at '{path}' is stored under a differently cased name, \
            watch it using its stored name instead"
        ))),
    }
}

/**
    Checks that a directory has an entry named exactly as requested.

    An exact entry always wins, so separate entries that only differ in case (including
    hard links between them) on case-sensitive filesystems are never confused. When there
    is no exact entry, the error carries the first entry with the same lowercase name, if
    any. That's only a hint for the message, not a claim about how the filesystem
    compares names.
*/
fn check_spelling(requested: &OsStr, entries: &[OsString]) -> Result<(), Option<OsString>> {
    if entries.iter().any(|entry| entry == requested) {
        return Ok(());
    }
    let lower = requested.to_string_lossy().to_lowercase();
    Err(entries
        .iter()
        .find(|entry| entry.to_string_lossy().to_lowercase() == lower)
        .cloned())
}

enum Received {
    Closed,
    Event(WatchEvent),
    HandleDropped,
    Stopped,
}

/**
    Yields watch events until the watcher gets stopped.
*/
struct EventSource {
    handle_dropped: bool,
    queue: EventQueue,
    shutdown: Receiver<()>,
    stopped: Rc<Cell<bool>>,
}

impl EventSource {
    async fn next(&mut self) -> Option<WatchEvent> {
        loop {
            if self.is_stopped() {
                return None;
            }
            if let Some(rescan) = self.queue.take_overflow() {
                return Some(rescan);
            }
            let received = if self.handle_dropped {
                match self.queue.recv().await {
                    Some(event) => Received::Event(event),
                    None => Received::Closed,
                }
            } else {
                let (shutdown_rx, queue) = (&self.shutdown, &self.queue);
                let shutdown = async {
                    match shutdown_rx.recv().await {
                        Ok(()) => Received::Stopped,
                        Err(_) => Received::HandleDropped,
                    }
                };
                let events = async {
                    match queue.recv().await {
                        Some(event) => Received::Event(event),
                        None => Received::Closed,
                    }
                };
                or(shutdown, events).await
            };
            match received {
                Received::Stopped | Received::Closed => return None,
                Received::HandleDropped => {
                    // The handle was garbage collected without being stopped (or
                    // the stop message was already consumed, checked at the top),
                    // meaning the user wants to keep watching forever
                    self.handle_dropped = true;
                }
                Received::Event(event) => return Some(event),
            }
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.get()
    }

    /**
        Resolves once the watcher has been stopped through its handle.

        Never resolves if the handle was garbage collected without being stopped.
    */
    async fn wait_for_stop(&self) {
        // NOTE: This may consume the stop message, in which case `next`
        // sees a closed channel with the stopped flag set and also ends
        self.shutdown.recv().await.ok();
        if !self.is_stopped() {
            future::pending::<()>().await;
        }
    }
}

async fn run_watcher(
    lua: Lua,
    watcher: RecommendedWatcher,
    mut source: EventSource,
    debounce: Duration,
    callback: LuaFunction,
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
                    Some(Some(event)) => push_debounced(&mut batch, event, QUEUE_CAPACITY),
                    Some(None) => return,
                    None => break,
                }
            }
        }

        for event in batch {
            if source.is_stopped() {
                return;
            }
            deliver(&lua, &callback, &source, event).await.ok();
        }
    }
}

/**
    Runs the callback for a single event on the scheduler, and waits for it
    to finish, so that callbacks never overlap even if they yield.

    Errors thrown inside the callback are reported by the scheduler.
*/
async fn deliver(
    lua: &Lua,
    callback: &LuaFunction,
    source: &EventSource,
    event: WatchEvent,
) -> LuaResult<()> {
    let thread_id = lua.push_thread_back(callback.clone(), event)?;
    lua.track_thread(thread_id);

    let finished = lua.wait_for_thread(thread_id);
    let stopped = source.wait_for_stop();

    // NOTE: If the watcher gets stopped while the callback is suspended, we stop
    // waiting for it, so that a callback that never resumes can not keep the
    // watcher (and the process) alive. Either way, taking the result untracks
    // the thread, and the scheduler discards results for untracked threads.
    or(finished, stopped).await;
    lua.get_thread_result(thread_id);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<OsString> {
        names.iter().map(OsString::from).collect()
    }

    #[test]
    fn check_spelling_accepts_exact_entries_only() {
        assert_eq!(
            check_spelling(OsStr::new("a.json"), &names(&["a.json"])),
            Ok(())
        );
        // Separate entries (or hard links) that only differ in case stay distinct
        assert_eq!(
            check_spelling(
                OsStr::new("config.json"),
                &names(&["CONFIG.JSON", "config.json"])
            ),
            Ok(())
        );
    }

    #[test]
    fn check_spelling_suggests_the_stored_name() {
        assert_eq!(
            check_spelling(OsStr::new("CONFIG.json"), &names(&["other", "config.json"])),
            Err(Some(OsString::from("config.json")))
        );
        assert_eq!(
            check_spelling(OsStr::new("Ä.json"), &names(&["ä.json"])),
            Err(Some(OsString::from("ä.json")))
        );
    }

    #[test]
    fn check_spelling_errors_without_a_hint_when_lowercasing_differs() {
        // Final and regular sigma lowercase differently, but some filesystems equate them
        assert_eq!(
            check_spelling(OsStr::new("ς.json"), &names(&["Σ.json"])),
            Err(None)
        );
    }

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
