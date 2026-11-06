use lune_utils::path::{VirtualPath, VirtualPathError};

use crate::{
    module::{ModuleError, ModuleId},
    source::SourceProvider,
};

const EXTENSIONS: [&str; 4] = ["js", "mjs", "ts", "mts"];

/** Resolves a runtime specifier against an original-source module ID. */
pub fn resolve_module(
    provider: &dyn SourceProvider,
    base: &ModuleId,
    specifier: &str,
) -> Result<ModuleId, ModuleError> {
    validate_specifier(specifier)?;
    let path = if specifier.starts_with('/') {
        let qualified = provider.qualify_absolute(specifier);
        VirtualPath::parse(&qualified)
            .map_err(|error| ModuleError::InvalidPath(error.to_string()))?
    } else {
        join_specifier(base, specifier)?
    };
    for candidate in resolution_candidates(&path, specifier.ends_with('/'))? {
        let candidate = ModuleId::new(candidate)?;
        if provider.exists(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(ModuleError::NotFound(format!(
        "\"{specifier}\" imported from \"{base}\""
    )))
}

/** Resolves and validates an absolute entrypoint ID without consulting the host filesystem directly. */
pub fn resolve_entry(provider: &dyn SourceProvider, path: &str) -> Result<ModuleId, ModuleError> {
    validate_specifier(path)?;
    if !path.starts_with('/') {
        return Err(ModuleError::InvalidSpecifier(format!(
            "The entrypoint \"{path}\" must be an absolute virtual path"
        )));
    }
    let virtual_path = VirtualPath::parse(path)
        .map_err(|error| ModuleError::InvalidPath(format!("{path} ({error})")))?;
    for candidate in resolution_candidates(&virtual_path, path.ends_with('/'))? {
        let id = ModuleId::new(candidate)?;
        if provider.exists(&id)? {
            return Ok(id);
        }
    }
    Err(ModuleError::NotFound(path.to_string()))
}

pub fn validate_specifier(specifier: &str) -> Result<(), ModuleError> {
    if specifier.is_empty() {
        return Err(ModuleError::InvalidSpecifier(
            "Module specifiers cannot be empty".into(),
        ));
    }
    if specifier.contains(['?', '#', '\0']) {
        return Err(ModuleError::InvalidSpecifier(format!(
            "Queries, fragments, and NUL bytes are not supported in \"{specifier}\""
        )));
    }
    if specifier.contains('\\') {
        return Err(ModuleError::InvalidSpecifier(format!(
            "Module specifiers must use portable forward slashes in \"{specifier}\""
        )));
    }
    if specifier.starts_with("//") || has_url_scheme(specifier) {
        return Err(ModuleError::InvalidSpecifier(format!(
            "URLs and runtime-specific schemes are not supported in \"{specifier}\""
        )));
    }
    if !specifier.starts_with(['/', '.']) {
        return Err(ModuleError::InvalidSpecifier(format!(
            "Bare package specifiers are not supported in \"{specifier}\""
        )));
    }
    if specifier.starts_with('.')
        && !specifier.starts_with("./")
        && !specifier.starts_with("../")
        && specifier != "."
        && specifier != ".."
    {
        return Err(ModuleError::InvalidSpecifier(format!(
            "Relative specifiers must start with \"./\" or \"../\" in \"{specifier}\""
        )));
    }
    Ok(())
}

fn has_url_scheme(specifier: &str) -> bool {
    let Some((scheme, _)) = specifier.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

fn join_specifier(base: &ModuleId, specifier: &str) -> Result<VirtualPath, ModuleError> {
    let result = if specifier.starts_with('/') {
        VirtualPath::parse(specifier)
    } else {
        base.parent().join(specifier)
    };
    result.map_err(|error| match error {
        VirtualPathError::AboveRoot => {
            ModuleError::RootEscape(format!("\"{specifier}\" imported from \"{base}\""))
        }
        _ => ModuleError::InvalidSpecifier(format!(
            "\"{specifier}\" imported from \"{base}\". {error}"
        )),
    })
}

pub(crate) fn resolution_candidates(
    path: &VirtualPath,
    directory_only: bool,
) -> Result<Vec<VirtualPath>, ModuleError> {
    let file_name = path.file_name().unwrap_or("");
    let extension = file_name
        .rfind('.')
        .filter(|index| *index > 0)
        .map(|index| &file_name[index + 1..]);
    if directory_only {
        return index_candidates(path);
    }
    if let Some(extension) = extension {
        let direct = ModuleId::new(path.clone())?;
        let mut candidates = vec![direct.virtual_path().clone()];
        let substitution = match extension {
            "js" => Some("ts"),
            "mjs" => Some("mts"),
            _ => None,
        };
        if let Some(substitution) = substitution {
            let substituted = path
                .as_str()
                .strip_suffix(extension)
                .expect("extension matched");
            candidates.push(
                VirtualPath::parse(format!("{substituted}{substitution}"))
                    .map_err(|error| ModuleError::InvalidPath(error.to_string()))?,
            );
        }
        return Ok(candidates);
    }

    let mut candidates = Vec::new();
    for extension in EXTENSIONS {
        candidates.push(
            VirtualPath::parse(format!("{}.{extension}", path.as_str()))
                .map_err(|error| ModuleError::InvalidPath(error.to_string()))?,
        );
    }
    candidates.extend(index_candidates(path)?);
    Ok(candidates)
}

fn index_candidates(path: &VirtualPath) -> Result<Vec<VirtualPath>, ModuleError> {
    EXTENSIONS
        .iter()
        .map(|extension| {
            path.join(format!("index.{extension}"))
                .map_err(|error| ModuleError::InvalidPath(error.to_string()))
        })
        .collect()
}
