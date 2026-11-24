use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use lune_utils::path::LuauFilePath;
use mlua::UserData;
use mlua::prelude::*;

/// Registry key for storing the current script path stack
const SCRIPT_PATH_STACK_KEY: &str = "__lune_script_path_stack";
/// Registry key for storing the files that entrypoint script paths resolved to
const SCRIPT_FILES_KEY: &str = "__lune_script_files";

/// What a `ScriptReference` points at.
#[derive(Debug, Clone)]
enum ReferenceTarget {
    /// A fixed path (used for script.Parent, child lookups, etc.)
    Static(PathBuf),
    /// The current script, resolved from the script path stack (used for the global `script`)
    CurrentScript,
    /// The nearest Git repository containing a script, resolved at access time. None means
    /// the current script (used for the global `project`).
    ProjectOf(Option<PathBuf>),
}

/// A reference to a script or module location in the file system.
/// This provides Roblox-like `script` and `project` globals that can be used for navigation.
#[derive(Debug, Clone)]
pub struct ScriptReference {
    target: ReferenceTarget,
}

impl ScriptReference {
    /// Create a new static script reference from a path
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            target: ReferenceTarget::Static(path.into()),
        }
    }

    /// Create a new dynamic script reference that resolves path at access time
    #[must_use]
    pub fn dynamic() -> Self {
        Self {
            target: ReferenceTarget::CurrentScript,
        }
    }

    /// Create a reference to the nearest Git repository containing the given script
    pub fn project_of(script_path: impl Into<PathBuf>) -> Self {
        Self {
            target: ReferenceTarget::ProjectOf(Some(script_path.into())),
        }
    }

    /// Create a reference to the nearest Git repository containing the current script
    #[must_use]
    pub fn dynamic_project() -> Self {
        Self {
            target: ReferenceTarget::ProjectOf(None),
        }
    }

    /// Get the path, resolving dynamically if needed
    fn resolve_path(&self, lua: &Lua) -> LuaResult<PathBuf> {
        match &self.target {
            ReferenceTarget::Static(p) => Ok(p.clone()),
            ReferenceTarget::CurrentScript => Ok(PathBuf::from(get_script_path_from_stack(lua)?)),
            ReferenceTarget::ProjectOf(script_path) => {
                let script_path = match script_path {
                    Some(p) => p.clone(),
                    None => PathBuf::from(get_script_path_from_stack(lua)?),
                };
                find_git_root(&script_file(lua, &script_path)).ok_or_else(|| {
                    LuaError::runtime(format!(
                        "No Git repository was found containing \"{}\".",
                        script_path.display()
                    ))
                })
            }
        }
    }

    /// Get the full path as a string, resolving dynamically if needed.
    ///
    /// # Errors
    ///
    /// Returns an error if a dynamic path cannot be resolved from the VM state.
    pub fn path_string(&self, lua: &Lua) -> LuaResult<String> {
        Ok(self.resolve_path(lua)?.display().to_string())
    }

    /// Resolve the actual existing source file or ordinary directory to an absolute path.
    fn full_path(&self, lua: &Lua) -> LuaResult<PathBuf> {
        let reference = self.resolve_path(lua)?;
        let path = script_file(lua, &reference);
        let resolved = if path != reference || path.is_file() {
            path
        } else {
            LuauFilePath::resolve(&path)
                .map_err(|error| {
                    LuaError::runtime(format!(
                        "Cannot resolve ScriptReference \"{}\". {error:?}.",
                        path.display()
                    ))
                })?
                .as_ref()
                .to_path_buf()
        };
        std::fs::metadata(&resolved)
            .and_then(|_| std::path::absolute(&resolved))
            .map_err(LuaError::external)
            .context(format!(
                "Cannot resolve ScriptReference \"{}\".",
                resolved.display()
            ))
    }

    /// Get just the file/folder name from a path
    fn name_from_path(path: &Path) -> String {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |s| s.to_string_lossy().to_string(),
        )
    }

    /// Get the parent directory as a new `ScriptReference` from a path
    fn parent_from_path(path: &Path) -> Option<ScriptReference> {
        path.parent().map(ScriptReference::new)
    }

    /// Get a child by name from a path
    /// First checks default.project.json for path mappings, then falls back to direct child
    fn child_from_path(path: &Path, name: &str) -> ScriptReference {
        let base_dir = if path.is_file() {
            path.parent().map(std::path::Path::to_path_buf)
        } else {
            Some(path.to_path_buf())
        };

        if let Some(base) = &base_dir {
            // Try to resolve through project.json first
            if let Some(resolved) = resolve_through_project(base, name) {
                return ScriptReference::new(resolved);
            }
        }

        // Fall back to direct child lookup
        let mut child_path = base_dir.unwrap_or_else(|| path.to_path_buf());
        child_path.push(name);
        ScriptReference::new(child_path)
    }
}

/// Remember the file an entrypoint resolved to. Entrypoint script paths are module paths,
/// which drop the extension and `init` segments and can be remapped by a
/// `default.project.json`, so the path alone doesn't say which folder the script is in.
///
/// # Errors
///
/// Returns an error if the VM's registered script-file table cannot be created or updated.
pub fn register_script_file(lua: &Lua, script_path: &str, file: &Path) -> LuaResult<()> {
    let files = if let Ok(t) = lua.named_registry_value::<LuaTable>(SCRIPT_FILES_KEY) {
        t
    } else {
        let t = lua.create_table()?;
        lua.set_named_registry_value(SCRIPT_FILES_KEY, t.clone())?;
        t
    };
    files.raw_set(script_path, file.display().to_string())
}

/// Get the file a script path refers to, which is the path itself unless it was registered.
pub fn script_file(lua: &Lua, script_path: &Path) -> PathBuf {
    lua.named_registry_value::<LuaTable>(SCRIPT_FILES_KEY)
        .ok()
        .and_then(|files| {
            files
                .raw_get::<Option<String>>(script_path.display().to_string())
                .ok()
                .flatten()
        })
        .map_or_else(|| script_path.to_path_buf(), PathBuf::from)
}

/// Find the nearest Git repository root containing the given script file by searching up
/// from its folder for a `.git` entry. `.git` is a directory in a normal checkout and a
/// file in worktrees and submodules, so both count.
fn find_git_root(script_file: &Path) -> Option<PathBuf> {
    // Make the path absolute first, since a bare relative file name has an empty parent.
    // Then canonicalize so that ".." components can't walk into a folder the script isn't
    // in. dunce avoids verbatim paths on Windows, which reject forward slashes.
    dunce::canonicalize(std::path::absolute(script_file).ok()?.parent()?)
        .ok()?
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Find the project root by searching up from the given path for default.project.json
fn find_project_root(start_path: &Path) -> Option<PathBuf> {
    let mut current = if start_path.is_file() {
        start_path.parent()?.to_path_buf()
    } else {
        start_path.to_path_buf()
    };

    loop {
        let project_file = current.join("default.project.json");
        if project_file.exists() {
            return Some(current);
        }

        current = current.parent()?.to_path_buf();
    }
}

/// Represents a node in the project tree
#[derive(Debug, Clone)]
struct ProjectNode {
    /// The $path if specified
    path: Option<String>,
    /// Child nodes
    children: HashMap<String, ProjectNode>,
}

impl ProjectNode {
    fn new() -> Self {
        Self {
            path: None,
            children: HashMap::new(),
        }
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let obj = value.as_object()?;
        let mut node = ProjectNode::new();

        // Check for $path
        if let Some(path_val) = obj.get("$path") {
            node.path = path_val.as_str().map(std::string::ToString::to_string);
        }

        // Process children (skip $ prefixed keys)
        for (key, child_value) in obj {
            if !key.starts_with('$')
                && let Some(child_node) = ProjectNode::from_json(child_value)
            {
                node.children.insert(key.clone(), child_node);
            }
        }

        Some(node)
    }

    fn at_physical_path(&self, root: &Path, current: &Path, target: &Path) -> Option<&Self> {
        if current == target {
            return Some(self);
        }
        self.children.iter().find_map(|(name, child)| {
            let child_path = child
                .path
                .as_ref()
                .map_or_else(|| current.join(name), |path| root.join(path));
            child.at_physical_path(root, &child_path, target)
        })
    }
}

/// Parse a default.project.json file and return the tree
fn parse_project_json(project_root: &Path) -> Option<ProjectNode> {
    let project_file = project_root.join("default.project.json");
    let content = fs::read_to_string(&project_file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Get the tree from the JSON
    let tree = json.get("tree")?;
    ProjectNode::from_json(tree)
}

/// Try to resolve a child name through the project.json tree
fn resolve_through_project(base_path: &Path, child_name: &str) -> Option<PathBuf> {
    // Find project root
    let project_root = find_project_root(base_path)?;

    // Parse project.json
    let tree = parse_project_json(&project_root)?;

    let current_node = tree
        .at_physical_path(&project_root, &project_root, base_path)
        .or_else(|| {
            tree.path.as_ref().and_then(|path| {
                tree.at_physical_path(&project_root, &project_root.join(path), base_path)
            })
        })?;

    // Now look for the child_name in the current node
    if let Some(child_node) = current_node.children.get(child_name)
        && let Some(ref path) = child_node.path
    {
        // Return the resolved path
        return Some(project_root.join(path));
    }

    None
}

impl UserData for ScriptReference {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // __tostring returns the full path
        methods.add_meta_method(LuaMetaMethod::ToString, |lua, this, ()| {
            let path = this.resolve_path(lua)?;
            Ok(path.display().to_string())
        });

        // String concatenation - use add_meta_function to handle both orderings
        // "string" .. script and script .. "string"
        methods.add_meta_function(
            LuaMetaMethod::Concat,
            |lua, (a, b): (LuaValue, LuaValue)| {
                // Determine which argument is the ScriptReference
                let (path_str, other, script_first) = if let LuaValue::UserData(ref ud) = a {
                    if let Ok(script_ref) = ud.borrow::<ScriptReference>() {
                        let path = script_ref.resolve_path(lua)?;
                        (path.display().to_string(), b, true)
                    } else {
                        return Err(LuaError::runtime(
                            "Invalid ScriptReference in concatenation",
                        ));
                    }
                } else if let LuaValue::UserData(ref ud) = b {
                    if let Ok(script_ref) = ud.borrow::<ScriptReference>() {
                        let path = script_ref.resolve_path(lua)?;
                        (path.display().to_string(), a, false)
                    } else {
                        return Err(LuaError::runtime(
                            "Invalid ScriptReference in concatenation",
                        ));
                    }
                } else {
                    return Err(LuaError::runtime(
                        "ScriptReference concatenation requires a ScriptReference",
                    ));
                };

                let other_str = match other {
                    LuaValue::String(s) => s.to_str()?.to_string(),
                    _ => other.to_string()?,
                };

                if script_first {
                    Ok(format!("{path_str}{other_str}"))
                } else {
                    Ok(format!("{other_str}{path_str}"))
                }
            },
        );

        // Allow accessing properties and children via indexing
        methods.add_meta_method(LuaMetaMethod::Index, |lua, this, key: String| {
            let path = this.resolve_path(lua)?;
            match key.as_str() {
                "Name" => {
                    let name = ScriptReference::name_from_path(&path);
                    Ok(LuaValue::String(lua.create_string(&name)?))
                }
                "Parent" => match ScriptReference::parent_from_path(&path) {
                    Some(parent) => Ok(LuaValue::UserData(lua.create_userdata(parent)?)),
                    None => Ok(LuaValue::Nil),
                },
                _ => {
                    // Treat as child lookup
                    let child = ScriptReference::child_from_path(&path, &key);
                    Ok(LuaValue::UserData(lua.create_userdata(child)?))
                }
            }
        });

        // GetFullName returns the actual absolute source file or ordinary directory.
        methods.add_method("GetFullName", |lua, this, ()| {
            Ok(this.full_path(lua)?.display().to_string())
        });
    }
}

/// Push a script path onto the stack (called when entering a module).
///
/// # Errors
///
/// Returns an error if the VM's script-path stack cannot be created or updated.
pub fn push_script_path(lua: &Lua, path: &str) -> LuaResult<()> {
    let stack = get_or_create_stack(lua)?;
    let len = stack.raw_len();
    stack.raw_set(len + 1, path)?;
    Ok(())
}

/// Pop a script path from the stack (called when exiting a module).
///
/// # Errors
///
/// Returns an error if the last entry cannot be removed from the VM's script-path stack.
pub fn pop_script_path(lua: &Lua) -> LuaResult<()> {
    if let Ok(stack) = lua.named_registry_value::<LuaTable>(SCRIPT_PATH_STACK_KEY) {
        let len = stack.raw_len();
        if len > 0 {
            stack.raw_set(len, LuaValue::Nil)?;
        }
    }
    Ok(())
}

/// Get the current script path from the stack.
///
/// # Errors
///
/// Returns an error if the current stack entry cannot be converted to a string.
pub fn get_current_script_path(lua: &Lua) -> LuaResult<Option<String>> {
    if let Ok(stack) = lua.named_registry_value::<LuaTable>(SCRIPT_PATH_STACK_KEY) {
        let len = stack.raw_len();
        if len > 0 {
            return stack.raw_get::<Option<String>>(len);
        }
    }
    Ok(None)
}

fn get_or_create_stack(lua: &Lua) -> LuaResult<LuaTable> {
    if let Ok(t) = lua.named_registry_value::<LuaTable>(SCRIPT_PATH_STACK_KEY) {
        Ok(t)
    } else {
        let t = lua.create_table()?;
        lua.set_named_registry_value(SCRIPT_PATH_STACK_KEY, t.clone())?;
        Ok(t)
    }
}

/// Returns the calling script's path, preferring the innermost Luau frame.
///
/// The registry stack can name a different script while a yielding require is in
/// flight, so callers that need the actual current file prefer frames and fall back
/// to the registry stack only when no frame is available.
///
/// # Errors
///
/// Returns an error if the fallback script-path stack entry cannot be read as a string.
pub fn get_calling_script_path(lua: &Lua) -> LuaResult<Option<String>> {
    Ok(get_script_path_from_frames(lua).or(get_current_script_path(lua)?))
}

/// Get the path of the script that's accessing a dynamic reference
fn get_script_path_from_stack(lua: &Lua) -> LuaResult<String> {
    // Check the calling frame before the registry stack. A require that yields leaves its
    // module on top of the registry stack while other threads run, so the registry can
    // name a different script than the one doing the access.
    if let Some(path) = get_script_path_from_frames(lua) {
        return Ok(path);
    }
    if let Some(path) = get_current_script_path(lua)? {
        return Ok(path);
    }
    Ok("[unknown]".to_string())
}

/// Get the path of the innermost Lua frame that belongs to a script file
fn get_script_path_from_frames(lua: &Lua) -> Option<String> {
    // Use mlua's native stack inspection instead of debug.info
    // We need to find the first Lua frame that has a valid source file
    // This tells us which script/module the current code was defined in
    for level in 0..100 {
        let result: Option<Option<String>> = lua.inspect_stack(level, |debug| {
            let source_info = debug.source();
            if let Some(source) = &source_info.source {
                // Skip C functions and internal code
                if source == "[C]" || source.starts_with("__mlua") {
                    return None;
                }
                // Strip @ or = prefix if present
                let path = if source.starts_with('@') || source.starts_with('=') {
                    source[1..].to_string()
                } else {
                    source.to_string()
                };
                // Skip internal identifiers that aren't file paths
                if !path.contains('/') && !path.contains('\\') {
                    return None;
                }
                return Some(path);
            }
            None
        });

        match result {
            None => break, // No more stack frames
            Some(Some(path)) => return Some(path),
            Some(None) => {}
        }
    }

    None
}

pub fn create(lua: Lua) -> LuaResult<LuaValue> {
    // Initialize the script path stack
    get_or_create_stack(&lua)?;

    // Create a dynamic script reference that resolves its path at access time
    // Using ScriptReference::dynamic() means typeof(script) returns "ScriptReference"
    let script = ScriptReference::dynamic();
    let userdata = lua.create_userdata(script)?;

    Ok(LuaValue::UserData(userdata))
}

pub fn create_project(lua: Lua) -> LuaResult<LuaValue> {
    get_or_create_stack(&lua)?;

    // Resolves to the Git repository of whichever script accesses it
    let project = ScriptReference::dynamic_project();
    let userdata = lua.create_userdata(project)?;

    Ok(LuaValue::UserData(userdata))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates an empty, canonicalized scratch directory unique to the given test
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lune-find-git-root-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dunce::canonicalize(dir).unwrap()
    }

    #[test]
    fn project_nodes_track_nested_physical_mappings() {
        let tree = ProjectNode::from_json(&serde_json::json!({
            "Alias": {
                "$path": "Source/Nested",
                "Leaf": { "$path": "Elsewhere/Actual.lua" }
            }
        }))
        .unwrap();
        let root = Path::new("project");
        let node = tree
            .at_physical_path(root, root, &root.join("Source/Nested"))
            .unwrap();
        assert_eq!(
            node.children["Leaf"].path.as_deref(),
            Some("Elsewhere/Actual.lua")
        );
        assert!(
            tree.at_physical_path(root, root, &root.join("Missing"))
                .is_none()
        );
    }

    #[test]
    fn find_git_root_finds_git_directory_above_file() {
        let root = scratch_dir("directory");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("Scripts")).unwrap();
        let script = root.join("Scripts").join("RunTests.luau");
        fs::write(&script, "").unwrap();

        assert_eq!(find_git_root(&script), Some(root.clone()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn find_git_root_finds_git_file_for_worktrees_and_submodules() {
        let root = scratch_dir("file");
        let worktree = root.join("Submodules").join("Child");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: ../../.git/modules/Child").unwrap();
        let script = worktree.join("Module.luau");
        fs::write(&script, "").unwrap();

        // The nearest repository wins over the outer one
        assert_eq!(find_git_root(&script), Some(worktree));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn find_git_root_ignores_folders_canceled_by_parent_components() {
        let root = scratch_dir("parent-components");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("Sibling").join(".git")).unwrap();
        fs::write(root.join("Main.luau"), "").unwrap();

        // Main lives next to Sibling, not inside it, so only the outer repository counts
        let script = root.join("Sibling").join("..").join("Main.luau");
        assert_eq!(find_git_root(&script), Some(root.clone()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn find_git_root_accepts_bare_relative_file_names() {
        // Unit tests run from the crate folder, which is inside the Lune repository
        let cwd = dunce::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let root = find_git_root(Path::new("Cargo.toml")).expect("should find the repository");
        assert!(cwd.starts_with(&root));
        assert!(root.join(".git").exists());
    }

    #[test]
    fn project_children_resolve_from_root_mapped_receivers() {
        let root = scratch_dir("root-mapped-receiver");
        fs::create_dir(root.join("Source")).unwrap();
        fs::write(
            root.join("default.project.json"),
            r#"{"tree":{"$path":"Source","Leaf":{"$path":"Elsewhere/Actual.lua"}}}"#,
        )
        .unwrap();
        for base in [&root, &root.join("Source")] {
            assert_eq!(
                resolve_through_project(base, "Leaf"),
                Some(root.join("Elsewhere/Actual.lua"))
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn full_path_resolves_files_directories_redirects_and_errors() {
        let root = scratch_dir("full-path");
        let lua = Lua::new();
        fs::create_dir_all(root.join("Folder")).unwrap();
        fs::create_dir_all(root.join("init")).unwrap();
        fs::create_dir_all(root.join("Module")).unwrap();
        fs::create_dir_all(root.join("LegacyModule")).unwrap();
        fs::create_dir_all(root.join("Collision")).unwrap();
        fs::create_dir_all(root.join("Package")).unwrap();
        for file in [
            "Plain.luau",
            "Legacy.lua",
            "Module/init.luau",
            "LegacyModule/init.lua",
            "Collision.luau",
            "Collision/init.luau",
            "Package/main.lua",
        ] {
            fs::write(root.join(file), "return true").unwrap();
        }
        fs::write(
            root.join("Package/default.project.json"),
            r#"{"tree":{"$path":"main.lua"}}"#,
        )
        .unwrap();

        for (reference, expected) in [
            ("Plain", "Plain.luau"),
            ("Plain.luau", "Plain.luau"),
            ("Legacy", "Legacy.lua"),
            ("Module", "Module/init.luau"),
            ("Module/init.luau", "Module/init.luau"),
            ("LegacyModule", "LegacyModule/init.lua"),
            ("Package", "Package/main.lua"),
            ("Folder", "Folder"),
            ("init", "init"),
        ] {
            let reference = ScriptReference::new(root.join(reference));
            assert_eq!(reference.full_path(&lua).unwrap(), root.join(expected));
        }
        assert!(
            ScriptReference::new(root.join("Missing"))
                .full_path(&lua)
                .is_err()
        );
        fs::write(root.join("Plain.lua"), "").unwrap();
        fs::write(root.join("Module/init.lua"), "").unwrap();
        for reference in ["Plain", "Module", "Collision"] {
            let error = ScriptReference::new(root.join(reference))
                .full_path(&lua)
                .unwrap_err();
            assert!(error.to_string().contains("Ambiguous"));
        }

        assert!(lune_utils::path::LuauModulePath::resolve(root.join("init")).is_err());

        // An explicitly registered entrypoint wins over extensionless ambiguity.
        let entrypoint = root.join("Plain");
        register_script_file(&lua, entrypoint.to_str().unwrap(), &root.join("Plain.luau")).unwrap();
        push_script_path(&lua, entrypoint.to_str().unwrap()).unwrap();
        assert_eq!(
            ScriptReference::dynamic().full_path(&lua).unwrap(),
            root.join("Plain.luau")
        );
        fs::remove_file(root.join("Plain.luau")).unwrap();
        assert!(ScriptReference::dynamic().full_path(&lua).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn script_file_prefers_registered_entrypoint_files() {
        let lua = Lua::new();
        let entrypoint = Path::new("/repo/Package");
        let module = Path::new("/repo/Source/Module.luau");

        // Unregistered paths are already files, so they come back unchanged
        assert_eq!(script_file(&lua, entrypoint), entrypoint);

        register_script_file(&lua, "/repo/Package", Path::new("/repo/Source/init.luau")).unwrap();
        assert_eq!(
            script_file(&lua, entrypoint),
            Path::new("/repo/Source/init.luau")
        );
        assert_eq!(script_file(&lua, module), module);
    }
}
