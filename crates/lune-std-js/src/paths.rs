use std::path::Path;

use lune_utils::path::LuauModulePath;

use crate::module::{ModuleError, ModuleId};

/// Namespaces embedded modules by their literal `ModuleRoot` so multiple roots
/// can coexist in one binary without colliding module IDs.
#[must_use]
pub fn module_root_key(root: Option<&str>) -> String {
    let Some(root) = root else {
        return String::new();
    };
    let mut root = root.replace('\\', "/");
    while let Some(stripped) = root.strip_prefix("./") {
        root = stripped.to_string();
    }
    let root = root.trim_matches('/');
    if root.is_empty() {
        String::new()
    } else {
        format!("/@{root}")
    }
}

/// Builds the synthetic importer identity for a Luau caller.
///
/// The caller's directory is represented inside the module namespace so relative
/// imports resolve exactly as they do at runtime. Callers outside `root` receive an
/// external identity and can still import absolute modules inside the root.
pub fn module_base_for_path(
    path: &Path,
    root: &Path,
    canonical_host: bool,
) -> Result<ModuleId, ModuleError> {
    let relative = if canonical_host {
        let canonical = if let Ok(canonical) = path.canonicalize() {
            canonical
        } else {
            let resolved = LuauModulePath::resolve(path).map_err(|error| {
                ModuleError::InvalidPath(format!("{} ({error:?})", path.display()))
            })?;
            let script_file = resolved.target().as_file().ok_or_else(|| {
                ModuleError::InvalidPath(format!(
                    "JavaScript imports require a Luau script file, not a directory: {}",
                    path.display()
                ))
            })?;
            script_file
                .canonicalize()
                .map_err(|error| ModuleError::Io(error.to_string()))?
        };
        canonical.strip_prefix(root).map(Path::to_path_buf).ok()
    } else {
        path.strip_prefix(root).map(Path::to_path_buf).ok()
    };
    let base = match relative {
        Some(relative) => {
            let parent = relative.parent().unwrap_or(Path::new(""));
            format!("/{}/__Luau.ts", parent.display())
        }
        None => "/__External/__Luau.ts".to_string(),
    };
    ModuleId::parse(base.replace('\\', "/"))
}
