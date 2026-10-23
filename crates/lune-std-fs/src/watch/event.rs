use std::path::{Path, PathBuf};

use mlua::prelude::*;
use notify::{EventKind, event::ModifyKind};

/**
    The kind of change that a watch event describes.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEventKind {
    Create,
    Modify,
    Remove,
    Rename,
    Other,
}

impl WatchEventKind {
    /**
        Maps a raw `notify` event kind to a watch event kind.

        Returns `None` for access events (opening, reading, closing), since those
        do not change anything on disk and would otherwise flood callbacks.
    */
    #[must_use]
    pub fn from_notify(kind: EventKind) -> Option<Self> {
        match kind {
            EventKind::Access(_) => None,
            EventKind::Create(_) => Some(Self::Create),
            EventKind::Modify(ModifyKind::Name(_)) => Some(Self::Rename),
            EventKind::Modify(_) => Some(Self::Modify),
            EventKind::Remove(_) => Some(Self::Remove),
            EventKind::Any | EventKind::Other => Some(Self::Other),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Modify => "modify",
            Self::Remove => "remove",
            Self::Rename => "rename",
            Self::Other => "other",
        }
    }
}

/**
    A single filesystem change, as delivered to a watch callback.
*/
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    pub kind: WatchEventKind,
    pub paths: Vec<PathBuf>,
}

impl IntoLua for WatchEvent {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let paths = lua.create_sequence_from(
            self.paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned()),
        )?;
        let event = lua.create_table_with_capacity(0, 2)?;
        event.set("kind", self.kind.as_str())?;
        event.set("paths", paths)?;
        Ok(LuaValue::Table(event))
    }
}

/**
    Describes what a watcher is watching, and turns raw `notify`
    events into the events that should reach the Luau callback.

    Watching a single file is implemented by watching its parent directory and
    filtering events down to the file. Watching the file itself would break as
    soon as an editor saves it through an atomic rename, since the original
    file (and any OS-level watch attached to it) gets replaced.
*/
#[derive(Debug, Clone)]
pub struct WatchTarget {
    canonical_root: Option<PathBuf>,
    file: Option<PathBuf>,
    root: PathBuf,
}

impl WatchTarget {
    /**
        Creates a new watch target.

        > `root` - The absolute directory that is watched at the OS level.
        > `canonical_root` - The canonical form of `root`, if it could be resolved.
        > `file` - The absolute path of the watched file, when watching a single file.
    */
    #[must_use]
    pub fn new(root: PathBuf, canonical_root: Option<PathBuf>, file: Option<PathBuf>) -> Self {
        Self {
            canonical_root,
            file,
            root,
        }
    }

    /**
        Rewrites a path reported by the OS so that it starts with the root
        path that was given to us, instead of its canonical form.

        Some backends (e.g. `FSEvents` on macOS) always report canonical paths,
        which would turn `/tmp/foo` into `/private/tmp/foo` and make paths
        differ between platforms and from what the user passed in.
    */
    #[must_use]
    pub fn normalize(&self, path: &Path) -> PathBuf {
        if path.starts_with(&self.root) {
            return path.to_path_buf();
        }
        if let Some(canonical_root) = &self.canonical_root
            && let Ok(rest) = path.strip_prefix(canonical_root)
        {
            if rest.as_os_str().is_empty() {
                return self.root.clone();
            }
            return self.root.join(rest);
        }
        path.to_path_buf()
    }

    /**
        Translates a raw `notify` event into a watch event.

        Returns `None` if the event should not be delivered, either because
        its kind is ignored or because it does not involve the watched file.
    */
    #[must_use]
    pub fn translate(&self, kind: EventKind, paths: &[PathBuf]) -> Option<WatchEvent> {
        let kind = WatchEventKind::from_notify(kind)?;
        let paths: Vec<PathBuf> = paths.iter().map(|path| self.normalize(path)).collect();
        if let Some(file) = &self.file
            && !paths.iter().any(|path| path == file)
        {
            return None;
        }
        Some(WatchEvent { kind, paths })
    }
}

/**
    Adds an event to a debounced batch, unless an identical event is already in it.

    Ordering of first occurrences is preserved.
*/
pub fn push_unique(batch: &mut Vec<WatchEvent>, event: WatchEvent) {
    if !batch.contains(&event) {
        batch.push(event);
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind, DataChange, MetadataKind, RemoveKind, RenameMode};

    use super::*;

    fn abs(path: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:\\{}", path.replace('/', "\\")))
        } else {
            PathBuf::from(format!("/{path}"))
        }
    }

    #[test]
    fn kinds_map_to_expected_strings() {
        let cases = [
            (EventKind::Access(AccessKind::Any), None),
            (EventKind::Create(CreateKind::File), Some("create")),
            (EventKind::Create(CreateKind::Folder), Some("create")),
            (
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                Some("modify"),
            ),
            (
                EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
                Some("modify"),
            ),
            (EventKind::Modify(ModifyKind::Any), Some("modify")),
            (
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                Some("rename"),
            ),
            (
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                Some("rename"),
            ),
            (EventKind::Remove(RemoveKind::File), Some("remove")),
            (EventKind::Any, Some("other")),
            (EventKind::Other, Some("other")),
        ];
        for (kind, expected) in cases {
            assert_eq!(
                WatchEventKind::from_notify(kind).map(WatchEventKind::as_str),
                expected,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn normalize_rewrites_canonical_prefix() {
        let target = WatchTarget::new(abs("tmp/dir"), Some(abs("private/tmp/dir")), None);
        assert_eq!(
            target.normalize(&abs("private/tmp/dir/a.txt")),
            abs("tmp/dir/a.txt")
        );
        assert_eq!(target.normalize(&abs("private/tmp/dir")), abs("tmp/dir"));
        assert_eq!(
            target.normalize(&abs("tmp/dir/a.txt")),
            abs("tmp/dir/a.txt")
        );
        assert_eq!(target.normalize(&abs("elsewhere/b")), abs("elsewhere/b"));
    }

    #[test]
    fn normalize_never_double_prefixes_paths_already_under_root() {
        // A root that is a symlink pointing to its own parent directory
        let target = WatchTarget::new(abs("a/b/link"), Some(abs("a/b")), None);
        assert_eq!(target.normalize(&abs("a/b/link/x")), abs("a/b/link/x"));
    }

    #[test]
    fn normalize_is_pure() {
        let target = WatchTarget::new(abs("tmp/dir"), Some(abs("private/tmp/dir")), None);
        let path = abs("private/tmp/dir/a.txt");
        assert_eq!(target.normalize(&path), target.normalize(&path));
    }

    #[test]
    fn directory_targets_deliver_every_non_access_event() {
        let target = WatchTarget::new(abs("dir"), None, None);
        let paths = vec![abs("dir/a.txt")];
        let event = target
            .translate(EventKind::Create(CreateKind::File), &paths)
            .unwrap();
        assert_eq!(event.kind, WatchEventKind::Create);
        assert_eq!(event.paths, paths);
        assert!(
            target
                .translate(EventKind::Access(AccessKind::Any), &paths)
                .is_none()
        );
        // Rescan events may come without any paths, and should still be delivered
        assert!(target.translate(EventKind::Other, &[]).is_some());
    }

    #[test]
    fn file_targets_only_deliver_events_for_the_file() {
        let file = abs("dir/config.json");
        let target = WatchTarget::new(abs("dir"), None, Some(file.clone()));
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Content));
        let rename = EventKind::Modify(ModifyKind::Name(RenameMode::Both));

        assert!(
            target
                .translate(modify, std::slice::from_ref(&file))
                .is_some()
        );
        assert!(target.translate(modify, &[abs("dir/other.json")]).is_none());
        assert!(
            target
                .translate(modify, &[abs("dir/sub/config.json")])
                .is_none()
        );
        assert!(target.translate(EventKind::Other, &[]).is_none());

        // Atomic saves rename a temporary file over the watched file
        let atomic = target
            .translate(rename, &[abs("dir/.config.json.tmp"), file.clone()])
            .unwrap();
        assert_eq!(atomic.kind, WatchEventKind::Rename);
        assert_eq!(atomic.paths, vec![abs("dir/.config.json.tmp"), file]);
    }

    #[test]
    fn file_targets_match_after_normalizing() {
        let target = WatchTarget::new(
            abs("tmp/dir"),
            Some(abs("private/tmp/dir")),
            Some(abs("tmp/dir/config.json")),
        );
        let event = target
            .translate(
                EventKind::Remove(RemoveKind::File),
                &[abs("private/tmp/dir/config.json")],
            )
            .unwrap();
        assert_eq!(event.paths, vec![abs("tmp/dir/config.json")]);
    }

    #[test]
    fn push_unique_drops_duplicates_and_keeps_order() {
        let event = |kind, path: &str| WatchEvent {
            kind,
            paths: vec![abs(path)],
        };
        let mut batch = Vec::new();
        push_unique(&mut batch, event(WatchEventKind::Modify, "a"));
        push_unique(&mut batch, event(WatchEventKind::Create, "b"));
        push_unique(&mut batch, event(WatchEventKind::Modify, "a"));
        push_unique(&mut batch, event(WatchEventKind::Modify, "b"));
        push_unique(&mut batch, event(WatchEventKind::Create, "b"));
        assert_eq!(
            batch,
            vec![
                event(WatchEventKind::Modify, "a"),
                event(WatchEventKind::Create, "b"),
                event(WatchEventKind::Modify, "b"),
            ]
        );
    }

    #[test]
    fn events_convert_to_lowercase_lua_tables() {
        let lua = Lua::new();
        let event = WatchEvent {
            kind: WatchEventKind::Rename,
            paths: vec![abs("a"), abs("b")],
        };
        let value = event.into_lua(&lua).unwrap();
        let table = value.as_table().unwrap();
        assert_eq!(table.get::<String>("kind").unwrap(), "rename");
        let paths: Vec<String> = table.get("paths").unwrap();
        assert_eq!(
            paths,
            vec![
                abs("a").to_string_lossy().into_owned(),
                abs("b").to_string_lossy().into_owned()
            ]
        );
    }
}
