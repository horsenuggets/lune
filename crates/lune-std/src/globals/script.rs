use std::path::{Path, PathBuf};

use mlua::prelude::*;
use mlua::UserData;

/// Registry key for storing the current script path stack
const SCRIPT_PATH_STACK_KEY: &str = "__lune_script_path_stack";

/// A reference to a script or module location in the file system.
/// This provides a Roblox-like `script` global that can be used for navigation.
#[derive(Debug, Clone)]
pub struct ScriptReference {
    /// The full path to this script/module
    path: PathBuf,
}

impl ScriptReference {
    /// Create a new script reference from a path
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Get the full path as a string
    pub fn path_string(&self) -> String {
        self.path.display().to_string()
    }

    /// Get just the file/folder name
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path_string())
    }

    /// Get the parent directory as a new ScriptReference
    pub fn parent(&self) -> Option<ScriptReference> {
        self.path.parent().map(|p| ScriptReference::new(p))
    }

    /// Get a child by name (file or folder)
    pub fn child(&self, name: &str) -> ScriptReference {
        let mut child_path = self.path.clone();

        // If current path points to a file, get its parent directory first
        if self.path.is_file() {
            if let Some(parent) = self.path.parent() {
                child_path = parent.to_path_buf();
            }
        }

        child_path.push(name);
        ScriptReference::new(child_path)
    }
}

impl UserData for ScriptReference {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, this, ()| {
            Ok(this.path_string())
        });

        methods.add_meta_method(LuaMetaMethod::Concat, |_, this, value: LuaValue| {
            match value {
                LuaValue::String(s) => Ok(format!("{}{}", this.path_string(), s.to_str()?)),
                _ => Ok(format!("{}{}", this.path_string(), value.to_string()?)),
            }
        });

        // Allow accessing properties and children via indexing
        methods.add_meta_method(LuaMetaMethod::Index, |lua, this, key: String| {
            match key.as_str() {
                "Name" => Ok(LuaValue::String(lua.create_string(&this.name())?)),
                "Path" => Ok(LuaValue::String(lua.create_string(&this.path_string())?)),
                "Parent" => {
                    match this.parent() {
                        Some(parent) => Ok(LuaValue::UserData(lua.create_userdata(parent)?)),
                        None => Ok(LuaValue::Nil),
                    }
                }
                _ => {
                    // Treat as child lookup
                    let child = this.child(&key);
                    Ok(LuaValue::UserData(lua.create_userdata(child)?))
                }
            }
        });
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
    // This should work better from within Rust callbacks
    for level in 0..100 {
        let result: Option<Option<String>> = lua.inspect_stack(level, |debug| {
            let source_info = debug.source();
            if let Some(source) = source_info.source {
                // Skip C functions and our internal code
                if source == "[C]" || source.contains("script_global") {
                    return None;
                }
                // Strip @ or = prefix if present
                let path = if source.starts_with('@') || source.starts_with('=') {
                    source[1..].to_string()
                } else {
                    source.to_string()
                };
                return Some(path);
            }
            None
        });

        match result {
            None => break, // No more stack frames
            Some(Some(path)) => return Ok(path), // Found a valid source
            Some(None) => continue, // Skip this frame
        }
    }

    Ok("[unknown]".to_string())
}

pub fn create(lua: Lua) -> LuaResult<LuaValue> {
    // Initialize the script path stack
    get_or_create_stack(&lua)?;

    // Create a dynamic script reference that resolves its path at access time
    // We use a table with a metatable for now, but the metamethods call into Rust
    let script_table = lua.create_table()?;

    // Create metatable with direct Rust implementations
    let metatable = lua.create_table()?;

    metatable.set(
        "__tostring",
        lua.create_function(|lua, _: LuaValue| {
            get_script_path_from_stack(lua)
        })?,
    )?;

    metatable.set(
        "__concat",
        lua.create_function(|lua, (a, b): (LuaValue, LuaValue)| {
            let path = get_script_path_from_stack(lua)?;
            match a {
                LuaValue::Table(_) | LuaValue::UserData(_) => {
                    // script .. "something"
                    Ok(format!("{}{}", path, b.to_string()?))
                }
                _ => {
                    // "something" .. script
                    Ok(format!("{}{}", a.to_string()?, path))
                }
            }
        })?,
    )?;

    metatable.set(
        "__index",
        lua.create_function(|lua, (_table, key): (LuaValue, String)| {
            let path = get_script_path_from_stack(lua)?;
            match key.as_str() {
                "Name" => {
                    let name = Path::new(&path)
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.clone());
                    Ok(LuaValue::String(lua.create_string(&name)?))
                }
                "Path" => Ok(LuaValue::String(lua.create_string(&path)?)),
                "Parent" => {
                    let parent_path = Path::new(&path)
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.clone());
                    let parent_ref = ScriptReference::new(parent_path);
                    Ok(LuaValue::UserData(lua.create_userdata(parent_ref)?))
                }
                _ => {
                    // Treat as child lookup - create a new ScriptReference
                    let child_path = Path::new(&path)
                        .parent()  // Get directory of current script
                        .map(|p| p.join(&key))
                        .unwrap_or_else(|| PathBuf::from(&key));
                    let child_ref = ScriptReference::new(child_path);
                    Ok(LuaValue::UserData(lua.create_userdata(child_ref)?))
                }
            }
        })?,
    )?;

    // Note: We intentionally don't set __type so that print(script) just shows
    // the path string (via __tostring) rather than <Script(path)>

    script_table.set_metatable(Some(metatable))?;

    Ok(LuaValue::Table(script_table))
}
