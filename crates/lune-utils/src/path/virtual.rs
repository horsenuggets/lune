use std::{fmt, path::Path, str::FromStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtualPathError {
    DriveRelative,
    MissingUncShare,
    NonUtf8,
    NotAbsolute,
    AboveRoot,
}

impl fmt::Display for VirtualPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DriveRelative => "drive-relative paths are not supported",
            Self::MissingUncShare => "UNC paths must include a server and share",
            Self::NonUtf8 => "virtual paths must be valid UTF-8",
            Self::NotAbsolute => "virtual paths must be absolute",
            Self::AboveRoot => "virtual paths cannot traverse above the root",
        })
    }
}

impl std::error::Error for VirtualPathError {}

/// An absolute, normalized path used to identify modules independently of the host OS.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtualPath(String);

impl VirtualPath {
    /// Parses and normalizes an absolute virtual path.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is relative, is not valid UTF-8, or traverses
    /// above the root.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, VirtualPathError> {
        let value = value.as_ref();
        if !value.starts_with('/') {
            return Err(VirtualPathError::NotAbsolute);
        }
        Self::normalize(value)
    }

    /// Converts an absolute host path into a portable virtual path.
    ///
    /// Drive prefixes and UNC server/share components are roots rather than path
    /// components, so they are omitted from the resulting virtual path.
    ///
    /// # Errors
    ///
    /// Returns an error for relative, drive-relative, malformed UNC, non-UTF-8, or
    /// root-escaping paths.
    pub fn from_host_path(path: impl AsRef<Path>) -> Result<Self, VirtualPathError> {
        let path = path.as_ref();
        let value = path.to_str().ok_or(VirtualPathError::NonUtf8)?;
        let value = value.replace('\\', "/");

        if let Some(unc) = value.strip_prefix("//") {
            let mut components = unc.split('/').filter(|part| !part.is_empty());
            let server = components.next();
            let share = components.next();
            if server.is_none() || share.is_none() {
                return Err(VirtualPathError::MissingUncShare);
            }
            let remainder = components.collect::<Vec<_>>().join("/");
            return Self::normalize(format!("/{remainder}"));
        }

        let bytes = value.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            if bytes.len() == 2 || bytes[2] != b'/' {
                return Err(VirtualPathError::DriveRelative);
            }
            return Self::normalize(&value[2..]);
        }

        Self::parse(value)
    }

    fn normalize(value: impl AsRef<str>) -> Result<Self, VirtualPathError> {
        let value = value.as_ref().replace('\\', "/");
        let mut components = Vec::new();
        for component in value.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    if components.pop().is_none() {
                        return Err(VirtualPathError::AboveRoot);
                    }
                }
                _ => components.push(component),
            }
        }
        Ok(Self(format!("/{}", components.join("/"))))
    }

    /// Appends a relative path and normalizes the result.
    ///
    /// # Errors
    ///
    /// Returns an error when the resulting path traverses above the root.
    pub fn join(&self, relative: impl AsRef<str>) -> Result<Self, VirtualPathError> {
        let relative = relative.as_ref();
        if relative.starts_with(['/', '\\']) {
            return Self::parse(relative);
        }
        Self::normalize(format!("{}/{}", self.0, relative))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the parent directory, or `None` for the root.
    ///
    /// # Panics
    ///
    /// Panics if the stored normalized path is not absolute.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.0 == "/" {
            return None;
        }
        let index = self.0.rfind('/').expect("absolute path contains a slash");
        Some(Self(if index == 0 {
            "/".to_string()
        } else {
            self.0[..index].to_string()
        }))
    }

    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        if self.0 == "/" {
            None
        } else {
            self.0.rsplit('/').next()
        }
    }

    #[must_use]
    pub fn starts_with(&self, other: &Self) -> bool {
        if other.0 == "/" {
            return true;
        }
        self.0
            .strip_prefix(&other.0)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

impl fmt::Display for VirtualPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for VirtualPath {
    type Err = VirtualPathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_components_and_separators() {
        let path = VirtualPath::parse("/Source/./Nested/../Module.ts").unwrap();
        assert_eq!(path.as_str(), "/Source/Module.ts");

        let path = VirtualPath::parse("/Source\\Nested\\Module.ts").unwrap();
        assert_eq!(path.as_str(), "/Source/Nested/Module.ts");
    }

    #[test]
    fn rejects_relative_paths_and_root_escape() {
        assert_eq!(
            VirtualPath::parse("Source/Module.ts"),
            Err(VirtualPathError::NotAbsolute)
        );
        assert_eq!(
            VirtualPath::parse("/../Module.ts"),
            Err(VirtualPathError::AboveRoot)
        );
        assert_eq!(
            VirtualPath::parse("/Source/../../Module.ts"),
            Err(VirtualPathError::AboveRoot)
        );
    }

    #[test]
    fn converts_windows_drive_paths() {
        let path = VirtualPath::from_host_path("C:\\Project\\Source\\Module.ts").unwrap();
        assert_eq!(path.as_str(), "/Project/Source/Module.ts");

        let path = VirtualPath::from_host_path("D:/Source/Module.ts").unwrap();
        assert_eq!(path.as_str(), "/Source/Module.ts");

        assert_eq!(
            VirtualPath::from_host_path("C:Source\\Module.ts"),
            Err(VirtualPathError::DriveRelative)
        );
    }

    #[test]
    fn converts_unc_paths() {
        let path = VirtualPath::from_host_path("\\\\server\\share\\Source\\Module.ts").unwrap();
        assert_eq!(path.as_str(), "/Source/Module.ts");

        assert_eq!(
            VirtualPath::from_host_path("\\\\server"),
            Err(VirtualPathError::MissingUncShare)
        );
    }

    #[test]
    fn joins_and_resolves_paths() {
        let parent = VirtualPath::parse("/Source/Nested").unwrap();
        assert_eq!(
            parent.join("../Module.ts").unwrap().as_str(),
            "/Source/Module.ts"
        );
        assert_eq!(
            parent.join("/Root/Module.ts").unwrap().as_str(),
            "/Root/Module.ts"
        );
    }

    #[test]
    fn returns_parents_and_file_names() {
        let path = VirtualPath::parse("/Source/Module.ts").unwrap();
        let parent = path.parent().unwrap();
        assert_eq!(parent.as_str(), "/Source");
        assert_eq!(parent.parent().unwrap().as_str(), "/");
        assert_eq!(parent.parent().unwrap().parent(), None);
        assert_eq!(path.file_name(), Some("Module.ts"));
        assert_eq!(parent.parent().unwrap().file_name(), None);
    }

    #[test]
    fn compares_whole_path_components() {
        let source = VirtualPath::parse("/Source").unwrap();
        assert!(
            VirtualPath::parse("/Source/Module.ts")
                .unwrap()
                .starts_with(&source)
        );
        assert!(
            !VirtualPath::parse("/SourceTwo/Module.ts")
                .unwrap()
                .starts_with(&source)
        );
        assert!(
            VirtualPath::parse("/Anything")
                .unwrap()
                .starts_with(&VirtualPath::parse("/").unwrap())
        );
    }
}
