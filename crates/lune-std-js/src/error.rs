use std::{error::Error, fmt};

use mlua::prelude::*;
use rquickjs::{CaughtError, Ctx, FromJs, convert::Coerced};

use crate::{loader::SharedModuleCache, module::ModuleId};

/**
    An error raised while crossing the JavaScript and Luau boundary.
*/
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsError {
    message: String,
}

impl JsError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn closed() -> Self {
        Self::new("JavaScript context is closed")
    }

    pub fn reentrant() -> Self {
        Self::new(
            "JavaScript context operations are not reentrant; another operation is already active",
        )
    }

    pub fn execution_limit() -> Self {
        Self::new("JavaScript execution limit exceeded")
    }

    pub fn operation_timeout() -> Self {
        Self::new("JavaScript operation timed out")
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for JsError {}

impl From<JsError> for LuaError {
    fn from(error: JsError) -> Self {
        LuaError::runtime(error.message)
    }
}

pub fn catch<T>(ctx: &Ctx<'_>, result: rquickjs::Result<T>) -> Result<T, JsError> {
    result.map_err(|error| format_caught_error(CaughtError::from_error(ctx, error)))
}

pub fn format_caught_error(error: CaughtError<'_>) -> JsError {
    format_caught_error_with_maps(error, None)
}

pub fn format_caught_error_with_maps(
    error: CaughtError<'_>,
    maps: Option<&SharedModuleCache>,
) -> JsError {
    match error {
        CaughtError::Error(error) => JsError::new(format!("JavaScript runtime error: {error}")),
        CaughtError::Exception(error) => {
            let message = error.message();
            let stack = error
                .stack()
                .map(|stack| maps.map_or_else(|| stack.clone(), |maps| remap_stack(&stack, maps)));
            match (message, stack) {
                (Some(message), Some(stack)) => {
                    JsError::new(format!("JavaScript exception: {message}\n{stack}"))
                }
                (Some(message), None) => JsError::new(format!("JavaScript exception: {message}")),
                (None, Some(stack)) => JsError::new(format!("JavaScript exception: {stack}")),
                (None, None) => JsError::new("JavaScript exception"),
            }
        }
        CaughtError::Value(value) => {
            let rendered = Coerced::<String>::from_js(value.ctx(), value.clone())
                .map_or_else(|_| format!("{value:?}"), |value| value.0);
            JsError::new(format!("JavaScript exception: {rendered}"))
        }
    }
}

/// Rewrites any generated JavaScript stack locations inside an existing error.
pub fn remap_error(error: JsError, maps: &SharedModuleCache) -> JsError {
    JsError::new(remap_stack(&error.message, maps))
}

/// Rewrites generated JavaScript stack locations back to their TypeScript sources.
fn remap_stack(stack: &str, maps: &SharedModuleCache) -> String {
    stack
        .lines()
        .map(|line| {
            let mut remapped = line.to_string();
            for token in line.split(['(', ')', ' ', '\t']) {
                let Some(location) = remap_location(token, maps) else {
                    continue;
                };
                remapped = remapped.replacen(token, &location, 1);
            }
            remapped
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn remap_location(token: &str, maps: &SharedModuleCache) -> Option<String> {
    let mut parts = token.rsplitn(3, ':');
    let column = parts.next()?.parse::<u32>().ok()?;
    let line = parts.next()?.parse::<u32>().ok()?;
    let path = parts.next()?;
    let id = ModuleId::parse(path).ok()?;
    let map = maps.source_map(&id)?;
    let position = map.original_position(line.saturating_sub(1), column.saturating_sub(1))?;
    Some(format!(
        "{}:{}:{}",
        position.module_id,
        position.line + 1,
        position.column + 1
    ))
}

pub fn throw(ctx: &Ctx<'_>, error: impl fmt::Display) -> rquickjs::Error {
    rquickjs::Exception::throw_message(ctx, &error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::transform_module;

    #[test]
    fn remaps_generated_frames_to_typescript() {
        let module = transform_module(
            ModuleId::parse("/Error.ts").unwrap(),
            "type Unused = string;\n\nexport function Fail(): never {\n    throw new Error(\"x\");\n}\n",
        )
        .unwrap();
        let cache = SharedModuleCache::new();
        cache.insert(module);
        let remapped = remap_stack("    at Fail (/Error.ts:2:5)", &cache);
        assert!(remapped.contains("/Error.ts:4:"), "{remapped}");
    }
}
