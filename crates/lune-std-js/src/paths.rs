use std::path::{Path, PathBuf};

use lune_utils::path::LuauModulePath;

use crate::module::{ModuleError, ModuleId};

/// Namespaces embedded modules by their literal `ModuleRoot` so multiple roots
/// can coexist in one binary without colliding module IDs.
#[must_use]
pub fn module_root_key(root: Option<&str>) -> String {
    let Some(root) = root else {
        return String::new();
    };
    format!("/@{}", encode_component(root))
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Returns whether `id` lies within the namespace at a component boundary.
#[must_use]
pub fn is_within_namespace(id: &str, namespace: &str) -> bool {
    namespace.is_empty()
        || id == namespace
        || id
            .strip_prefix(namespace)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Normalizes a virtual path using only path components.
#[must_use]
pub fn normalize_virtual(path: &Path) -> PathBuf {
    let mut parts = Vec::new();
    let normalized = path.to_string_lossy().replace('\\', "/");
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            component => parts.push(component),
        }
    }
    PathBuf::from(format!("/{}", parts.join("/")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_distinct_module_root_keys() {
        assert_eq!(module_root_key(None), "");
        assert_ne!(module_root_key(None), module_root_key(Some("..")));
        assert_ne!(
            module_root_key(Some("./Source")),
            module_root_key(Some("Source"))
        );
        assert!(module_root_key(Some("./Source")).starts_with("/@"));
        assert!(!module_root_key(Some("./Source/../Other")).contains("/@/"));
    }

    #[test]
    fn checks_namespace_boundaries() {
        assert!(is_within_namespace("/@A/Value.ts", "/@A"));
        assert!(!is_within_namespace("/@AB/Value.ts", "/@A"));
        assert!(is_within_namespace("/@AB/Value.ts", "/@AB"));
        assert!(is_within_namespace("/anything", ""));
    }
}
