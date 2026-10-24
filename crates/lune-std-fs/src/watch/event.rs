use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

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

impl WatchEvent {
    /**
        Creates an event without any paths, signaling that
        changes may have been missed and should be rescanned.
    */
    #[must_use]
    pub fn rescan() -> Self {
        Self {
            kind: WatchEventKind::Other,
            paths: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_rescan(&self) -> bool {
        self.kind == WatchEventKind::Other && self.paths.is_empty()
    }
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
    case_insensitive: bool,
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
            case_insensitive: false,
            file,
            root,
        }
    }

    /**
        Sets whether the watched file's name should be matched without regard to case.

        On case-insensitive filesystems (the default on Windows and macOS), a file can be
        watched using any casing, and renamed to a different casing later on, while the OS
        always reports the casing it is currently stored with.
    */
    #[must_use]
    pub fn with_case_insensitive(mut self, case_insensitive: bool) -> Self {
        self.case_insensitive = case_insensitive;
        self
    }

    fn is_watched_file(&self, path: &Path) -> bool {
        let Some(file) = &self.file else {
            return false;
        };
        if path == file {
            return true;
        }
        self.case_insensitive
            && path.parent() == file.parent()
            && match (path.file_name(), file.file_name()) {
                (Some(a), Some(b)) => names_equal_ignoring_case(a, b),
                _ => false,
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

        Events without any paths are always delivered, since they signal that
        changes may have been missed and that everything should be rescanned.
    */
    #[must_use]
    pub fn translate(&self, kind: EventKind, paths: &[PathBuf]) -> Option<WatchEvent> {
        let kind = WatchEventKind::from_notify(kind)?;
        let paths: Vec<PathBuf> = paths.iter().map(|path| self.normalize(path)).collect();
        if self.file.is_some()
            && !paths.is_empty()
            && !paths.iter().any(|path| self.is_watched_file(path))
        {
            return None;
        }
        Some(WatchEvent { kind, paths })
    }
}

/**
    Checks if two file names are equal when ignoring (Unicode) case.
*/
#[must_use]
pub fn names_equal_ignoring_case(a: &OsStr, b: &OsStr) -> bool {
    a == b || a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/**
    Returns the given file name with its case flipped, or `None` if
    it has no letters with case, and its case can not be flipped.

    Used to probe whether a filesystem treats names case-insensitively.
*/
#[must_use]
pub fn flip_case(name: &OsStr) -> Option<OsString> {
    let name = name.to_str()?;
    let upper = name.to_uppercase();
    let flipped = if upper == name {
        name.to_lowercase()
    } else {
        upper
    };
    (flipped != name).then(|| OsString::from(flipped))
}

/**
    Adds an event to a debounced batch, keeping the batch small and ordered.

    > Identical events are merged into their latest occurrence, so the batch
    > still ends with the most recent state (e.g. create, remove, create).
    > Rescan events act as a barrier, discarding every older event in the batch.
    > A batch that grows beyond `capacity` is replaced with a single rescan event.
*/
pub fn push_debounced(batch: &mut Vec<WatchEvent>, event: WatchEvent, capacity: usize) {
    if event.is_rescan() {
        batch.clear();
        batch.push(event);
        return;
    }
    batch.retain(|existing| existing != &event);
    batch.push(event);
    if batch.len() > capacity {
        batch.clear();
        batch.push(WatchEvent::rescan());
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
        // Rescan events have no paths, and must reach file watchers too
        assert!(target.translate(EventKind::Other, &[]).is_some());

        // Atomic saves rename a temporary file over the watched file
        let atomic = target
            .translate(rename, &[abs("dir/.config.json.tmp"), file.clone()])
            .unwrap();
        assert_eq!(atomic.kind, WatchEventKind::Rename);
        assert_eq!(atomic.paths, vec![abs("dir/.config.json.tmp"), file]);
    }

    #[test]
    fn case_insensitive_file_targets_match_any_casing() {
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Content));
        let cases = [
            // (watched name, reported path, case insensitive, expected match)
            ("CONFIG.json", "dir/config.json", true, true),
            ("config.json", "dir/CONFIG.JSON", true, true),
            ("Ä.json", "dir/ä.json", true, true),
            ("CONFIG.json", "dir/sub/config.json", true, false),
            ("CONFIG.json", "dir/other.json", true, false),
            ("CONFIG.json", "dir/config.json", false, false),
            ("config.json", "dir/config.json", false, true),
        ];
        for (watched, reported, case_insensitive, expected) in cases {
            let target = WatchTarget::new(abs("dir"), None, Some(abs("dir").join(watched)))
                .with_case_insensitive(case_insensitive);
            assert_eq!(
                target.translate(modify, &[abs(reported)]).is_some(),
                expected,
                "watched {watched}, reported {reported}, case insensitive {case_insensitive}"
            );
        }
    }

    #[test]
    fn case_insensitive_matching_survives_case_only_renames() {
        let target = WatchTarget::new(abs("dir"), None, Some(abs("dir/CONFIG.json")))
            .with_case_insensitive(true);
        let rename = EventKind::Modify(ModifyKind::Name(RenameMode::Both));
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Content));
        assert!(
            target
                .translate(rename, &[abs("dir/config.json"), abs("dir/Config.json")])
                .is_some()
        );
        assert!(
            target
                .translate(modify, &[abs("dir/Config.json")])
                .is_some()
        );
    }

    #[test]
    fn names_compare_with_unicode_case_folding() {
        assert!(names_equal_ignoring_case(
            OsStr::new("a.TXT"),
            OsStr::new("A.txt")
        ));
        assert!(names_equal_ignoring_case(
            OsStr::new("Ä.json"),
            OsStr::new("ä.json")
        ));
        assert!(!names_equal_ignoring_case(
            OsStr::new("a.txt"),
            OsStr::new("b.txt")
        ));
    }

    #[test]
    fn flip_case_only_flips_names_with_cased_letters() {
        assert_eq!(flip_case(OsStr::new("config.json")).unwrap(), "CONFIG.JSON");
        assert_eq!(flip_case(OsStr::new("Config.json")).unwrap(), "CONFIG.JSON");
        assert_eq!(flip_case(OsStr::new("CONFIG")).unwrap(), "config");
        assert_eq!(flip_case(OsStr::new("ä.json")).unwrap(), "Ä.JSON");
        assert!(flip_case(OsStr::new("123-456")).is_none());
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

    fn event(kind: WatchEventKind, path: &str) -> WatchEvent {
        WatchEvent {
            kind,
            paths: vec![abs(path)],
        }
    }

    fn debounce(events: Vec<WatchEvent>, capacity: usize) -> Vec<WatchEvent> {
        let mut batch = Vec::new();
        for event in events {
            push_debounced(&mut batch, event, capacity);
        }
        batch
    }

    #[test]
    fn push_debounced_merges_duplicates_into_latest_occurrence() {
        use WatchEventKind::{Create, Modify};
        let batch = debounce(
            vec![
                event(Modify, "a"),
                event(Create, "b"),
                event(Modify, "a"),
                event(Modify, "b"),
                event(Create, "b"),
            ],
            16,
        );
        assert_eq!(
            batch,
            vec![event(Modify, "a"), event(Modify, "b"), event(Create, "b")]
        );
    }

    #[test]
    fn push_debounced_keeps_the_final_state_of_a_path() {
        use WatchEventKind::{Create, Remove};
        let batch = debounce(
            vec![event(Create, "a"), event(Remove, "a"), event(Create, "a")],
            16,
        );
        assert_eq!(batch, vec![event(Remove, "a"), event(Create, "a")]);
        assert_eq!(batch.last(), Some(&event(Create, "a")));
    }

    #[test]
    fn push_debounced_treats_rescans_as_barriers() {
        use WatchEventKind::{Create, Modify};
        let batch = debounce(
            vec![
                event(Create, "a"),
                WatchEvent::rescan(),
                event(Modify, "b"),
                WatchEvent::rescan(),
                event(Create, "c"),
            ],
            16,
        );
        assert_eq!(batch, vec![WatchEvent::rescan(), event(Create, "c")]);
    }

    #[test]
    fn push_debounced_never_exceeds_capacity() {
        let capacity = 4;
        let mut batch = Vec::new();
        for index in 0..100 {
            push_debounced(
                &mut batch,
                event(WatchEventKind::Create, &format!("file{index}")),
                capacity,
            );
            assert!(batch.len() <= capacity, "batch grew to {}", batch.len());
        }
        // Overflowing replaces everything older with a rescan barrier
        assert_eq!(batch.first(), Some(&WatchEvent::rescan()));
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
