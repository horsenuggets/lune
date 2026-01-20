use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use mlua::UserData;
use mlua::prelude::*;

/// Registry key for storing the current script path stack
const SCRIPT_PATH_STACK_KEY: &str = "__lune_script_path_stack";

/// A reference to a script or module location in the file system.
/// This provides a Roblox-like `script` global that can be used for navigation.
///
/// Can be either:
/// - Static: contains a fixed path (used for script.Parent, child lookups, etc.)
/// - Dynamic: resolves path from the current script path stack (used for the global `script`)
#[derive(Debug, Clone)]
pub struct ScriptReference {
    /// The path to this script/module.
    /// None means this is a dynamic reference that should look up the current path.
    path: Option<PathBuf>,
}

impl ScriptReference {
    /// Create a new static script reference from a path
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
        }
    }

    /// Create a new dynamic script reference that resolves path at access time
    pub fn dynamic() -> Self {
        Self { path: None }
    }

    /// Get the path, resolving dynamically if needed
    fn resolve_path(&self, lua: &Lua) -> LuaResult<PathBuf> {
        match &self.path {
            Some(p) => Ok(p.clone()),
            None => {
                let path_str = get_script_path_from_stack(lua)?;
                Ok(PathBuf::from(path_str))
            }
        }
    }

    /// Get the full path as a string (for static references only)
    pub fn path_string(&self) -> String {
        self.path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[dynamic]".to_string())
    }

    /// Get just the file/folder name from a path
    fn name_from_path(path: &Path) -> String {
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    }

    /// Get the parent directory as a new ScriptReference from a path
    fn parent_from_path(path: &Path) -> Option<ScriptReference> {
        path.parent().map(|p| ScriptReference::new(p))
    }

    /// Get a child by name from a path
    /// First checks default.project.json for path mappings, then falls back to direct child
    fn child_from_path(path: &Path, name: &str) -> ScriptReference {
        let base_dir = if path.is_file() {
            path.parent().map(|p| p.to_path_buf())
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

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return None,
        }
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
            node.path = path_val.as_str().map(|s| s.to_string());
        }

        // Process children (skip $ prefixed keys)
        for (key, child_value) in obj.iter() {
            if !key.starts_with('$') {
                if let Some(child_node) = ProjectNode::from_json(child_value) {
                    node.children.insert(key.clone(), child_node);
                }
            }
        }

        Some(node)
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

    // Calculate relative path from project root to base_path
    let relative_to_root = base_path.strip_prefix(&project_root).ok()?;

    // Navigate the tree to find current position
    let mut current_node = &tree;

    // Navigate through the relative path to find current node
    for component in relative_to_root.components() {
        let name = component.as_os_str().to_string_lossy();

        // First, check if there's a direct child with this name
        if let Some(child) = current_node.children.get(name.as_ref()) {
            current_node = child;
            continue;
        }

        // Check if any child has a $path that matches
        let mut found = false;
        for (_, child) in &current_node.children {
            if let Some(ref path) = child.path {
                let resolved_path = project_root.join(path);
                if resolved_path == base_path.join(component.as_os_str()) {
                    current_node = child;
                    found = true;
                    break;
                }
            }
        }

        if !found {
            // Can't navigate further in the tree
            return None;
        }
    }

    // Now look for the child_name in the current node
    if let Some(child_node) = current_node.children.get(child_name) {
        if let Some(ref path) = child_node.path {
            // Return the resolved path
            return Some(project_root.join(path));
        }
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
                        let path = script_ref.resolve_path(&lua)?;
                        (path.display().to_string(), b, true)
                    } else {
                        return Err(LuaError::runtime(
                            "Invalid ScriptReference in concatenation",
                        ));
                    }
                } else if let LuaValue::UserData(ref ud) = b {
                    if let Ok(script_ref) = ud.borrow::<ScriptReference>() {
                        let path = script_ref.resolve_path(&lua)?;
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
                    Ok(format!("{}{}", path_str, other_str))
                } else {
                    Ok(format!("{}{}", other_str, path_str))
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

        // GetFullName returns the full path (like Roblox's Instance:GetFullName())
        methods.add_method("GetFullName", |lua, this, ()| {
            let path = this.resolve_path(lua)?;
            Ok(path.display().to_string())
        });

        // RequirePath returns a relative path string for use with require()
        // Usage: require(script.Parent.Module:RequirePath())
        methods.add_method("RequirePath", |lua, this, ()| {
            let path = this.resolve_path(lua)?;
            let path_str = path.display().to_string();
            // Get current script path to calculate relative path
            if let Some(current_script) = get_current_script_path(lua)? {
                let relative = make_relative_path(&current_script, &path_str);
                Ok(relative)
            } else {
                // Fallback: just use ./ prefix with the path
                Ok(format!("./{}", path_str))
            }
        });
    }
}

/// Convert an absolute target path to a relative path from the current script.
/// Returns a path starting with "./" or "../" as required by the require system.
fn make_relative_path(current_script: &str, target_path: &str) -> String {
    let current = Path::new(current_script);
    let target = Path::new(target_path);

    // Get the directory of the current script
    let current_dir = current.parent().unwrap_or(current);

    // Try to make target relative to current_dir
    if let Ok(relative) = target.strip_prefix(current_dir) {
        // Target is inside current directory
        format!("./{}", relative.display())
    } else {
        // Need to go up directories - find common ancestor
        let current_parts: Vec<_> = current_dir.components().collect();
        let target_parts: Vec<_> = target.components().collect();

        // Find how many components match
        let common_len = current_parts
            .iter()
            .zip(target_parts.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // Calculate how many ".." we need
        let ups_needed = current_parts.len() - common_len;

        // Build the relative path
        let mut result = String::new();
        if ups_needed == 0 {
            result.push_str("./");
        } else {
            for _ in 0..ups_needed {
                result.push_str("../");
            }
        }

        // Add the remaining target path components
        for (i, part) in target_parts.iter().skip(common_len).enumerate() {
            if i > 0 {
                result.push('/');
            }
            result.push_str(&part.as_os_str().to_string_lossy());
        }

        result
    }
}

/// Push a script path onto the stack (called when entering a module)
pub fn push_script_path(lua: &Lua, path: &str) -> LuaResult<()> {
    let stack = get_or_create_stack(lua)?;
    let len = stack.raw_len();
    stack.raw_set(len + 1, path)?;
    Ok(())
}

/// Pop a script path from the stack (called when exiting a module)
pub fn pop_script_path(lua: &Lua) -> LuaResult<()> {
    if let Ok(stack) = lua.named_registry_value::<LuaTable>(SCRIPT_PATH_STACK_KEY) {
        let len = stack.raw_len();
        if len > 0 {
            stack.raw_set(len, LuaValue::Nil)?;
        }
    }
    Ok(())
}

/// Get the current script path from the stack
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
    match lua.named_registry_value::<LuaTable>(SCRIPT_PATH_STACK_KEY) {
        Ok(t) => Ok(t),
        Err(_) => {
            let t = lua.create_table()?;
            lua.set_named_registry_value(SCRIPT_PATH_STACK_KEY, t.clone())?;
            Ok(t)
        }
    }
}

/// Get the current script path by walking the Lua call stack
fn get_script_path_from_stack(lua: &Lua) -> LuaResult<String> {
    // First check the registry stack (for required modules)
    if let Some(path) = get_current_script_path(lua)? {
        return Ok(path);
    }

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
            Some(Some(path)) => return Ok(path),
            Some(None) => continue, // Skip this frame
        }
    }

    Ok("[unknown]".to_string())
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
