use std::{error::Error, fmt};

use mlua::prelude::*;
use rquickjs::{CaughtError, Ctx, FromJs, convert::Coerced};

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
    match error {
        CaughtError::Error(error) => JsError::new(format!("JavaScript runtime error: {error}")),
        CaughtError::Exception(error) => {
            let message = error.message();
            let stack = error.stack();
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

pub fn throw(ctx: &Ctx<'_>, error: impl fmt::Display) -> rquickjs::Error {
    rquickjs::Exception::throw_message(ctx, &error.to_string())
}
