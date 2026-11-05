use std::{fmt, path::Path, str::FromStr};

use lune_utils::path::{VirtualPath, VirtualPathError};

use crate::source_map::PortableSourceMap;

/** The original-source formats accepted by the JavaScript module subsystem. */
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleKind {
    JavaScript,
    JavaScriptModule,
    TypeScript,
    TypeScriptModule,
}

impl ModuleKind {
    #[must_use]
    pub fn from_path(path: &str) -> Option<Self> {
        let extension = Path::new(path).extension()?.to_str()?;
        match extension {
            "js" => Some(Self::JavaScript),
            "mjs" => Some(Self::JavaScriptModule),
            "ts" => Some(Self::TypeScript),
            "mts" => Some(Self::TypeScriptModule),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_typescript(self) -> bool {
        matches!(self, Self::TypeScript | Self::TypeScriptModule)
    }
}

/** A canonical, root-relative virtual path to an original JavaScript or TypeScript module. */
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleId(VirtualPath);

impl ModuleId {
    pub fn new(path: VirtualPath) -> Result<Self, ModuleError> {
        if ModuleKind::from_path(path.as_str()).is_none() {
            return Err(ModuleError::UnsupportedExtension(path.to_string()));
        }
        Ok(Self(path))
    }

    pub fn parse(path: impl AsRef<str>) -> Result<Self, ModuleError> {
        let path = path.as_ref();
        let virtual_path = VirtualPath::parse(path).map_err(|error| match error {
            VirtualPathError::AboveRoot => ModuleError::RootEscape(path.to_string()),
            _ => ModuleError::InvalidPath(format!("{path} ({error})")),
        })?;
        Self::new(virtual_path)
    }

    #[must_use]
    pub fn kind(&self) -> ModuleKind {
        ModuleKind::from_path(self.as_str()).expect("module IDs always have a supported extension")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn virtual_path(&self) -> &VirtualPath {
        &self.0
    }

    #[must_use]
    pub fn parent(&self) -> VirtualPath {
        self.0
            .parent()
            .expect("a module file always has a parent directory")
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ModuleId {
    type Err = ModuleError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleError {
    ImportAttributes(String),
    InvalidPath(String),
    InvalidSpecifier(String),
    Io(String),
    NotFound(String),
    NotPreloaded(String),
    Parse(String),
    Provider(String),
    RootEscape(String),
    SymlinkEscape(String),
    Transform(String),
    UnsupportedExtension(String),
    UnresolvedImport(String),
}

impl fmt::Display for ModuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (label, message) = match self {
            Self::ImportAttributes(message) => ("Import attributes are not supported", message),
            Self::InvalidPath(message) => ("Invalid module path", message),
            Self::InvalidSpecifier(message) => ("Invalid module specifier", message),
            Self::Io(message) => ("Module I/O failed", message),
            Self::NotFound(message) => ("Module not found", message),
            Self::NotPreloaded(message) => ("Module is not embedded", message),
            Self::Parse(message) => ("Module parsing failed", message),
            Self::Provider(message) => ("Source provider failed", message),
            Self::RootEscape(message) => ("Module path escapes the source root", message),
            Self::SymlinkEscape(message) => ("Module symlink escapes the source root", message),
            Self::Transform(message) => ("Module transformation failed", message),
            Self::UnsupportedExtension(message) => ("Unsupported module extension", message),
            Self::UnresolvedImport(message) => ("Unresolved module import", message),
        };
        write!(formatter, "{label}: {message}")
    }
}

impl std::error::Error for ModuleError {}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DependencyKind {
    Static,
    Dynamic,
}

/** A statically discoverable dependency. Computed dynamic imports are resolved at runtime. */
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ModuleDependency {
    pub kind: DependencyKind,
    pub specifier: String,
}

/** Generated ESM, its portable map, and dependencies discovered with Oxc. */
#[derive(Clone, Debug)]
pub struct TransformedModule {
    pub(crate) code: String,
    pub(crate) dependencies: Vec<ModuleDependency>,
    pub(crate) has_computed_dynamic_import: bool,
    pub(crate) id: ModuleId,
    pub(crate) source_map: Option<PortableSourceMap>,
}

impl TransformedModule {
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub fn dependencies(&self) -> &[ModuleDependency] {
        &self.dependencies
    }

    #[must_use]
    pub fn has_computed_dynamic_import(&self) -> bool {
        self.has_computed_dynamic_import
    }

    #[must_use]
    pub fn id(&self) -> &ModuleId {
        &self.id
    }

    #[must_use]
    pub fn source_map(&self) -> Option<&PortableSourceMap> {
        self.source_map.as_ref()
    }

    #[must_use]
    pub fn into_bundle_parts(self) -> (ModuleId, String, Option<PortableSourceMap>) {
        (self.id, self.code, self.source_map)
    }
}
