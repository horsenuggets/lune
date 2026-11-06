use std::{path::PathBuf, time::Duration};

use mlua::prelude::*;
use rquickjs::AsyncRuntime;

const DEFAULT_JOB_BUDGET: usize = 64;

/**
    Options for creating a JavaScript context.
*/
#[derive(Debug, Clone)]
pub struct JsContextOptions {
    pub(crate) memory_limit_bytes: Option<usize>,
    pub(crate) stack_limit_bytes: Option<usize>,
    pub(crate) execution_limit: Option<Duration>,
    pub(crate) operation_timeout: Option<Duration>,
    pub(crate) job_budget: usize,
    pub(crate) module_root: Option<PathBuf>,
}

impl Default for JsContextOptions {
    fn default() -> Self {
        Self {
            memory_limit_bytes: None,
            stack_limit_bytes: None,
            execution_limit: None,
            operation_timeout: None,
            job_budget: DEFAULT_JOB_BUDGET,
            module_root: None,
        }
    }
}

impl JsContextOptions {
    pub async fn apply(&self, runtime: &AsyncRuntime) {
        if let Some(limit) = self.memory_limit_bytes {
            runtime.set_memory_limit(limit).await;
        }
        if let Some(limit) = self.stack_limit_bytes {
            runtime.set_max_stack_size(limit).await;
        }
    }
}

/**
    Options for one JavaScript evaluation.
*/
#[derive(Debug, Default, Clone)]
pub struct JsEvaluateOptions {
    pub(crate) filename: Option<String>,
    pub(crate) module: bool,
}

impl FromLua for JsContextOptions {
    fn from_lua(value: LuaValue, _: &Lua) -> LuaResult<Self> {
        let LuaValue::Table(table) = value else {
            return if value == LuaValue::Nil {
                Ok(Self::default())
            } else {
                Err(LuaError::FromLuaConversionError {
                    from: value.type_name(),
                    to: "JsContextOptions".to_string(),
                    message: Some(format!(
                        "Invalid context options; expected table, got {}",
                        value.type_name()
                    )),
                })
            };
        };

        let mut options = Self::default();
        for pair in table.pairs::<LuaValue, LuaValue>() {
            let (key, value) = pair?;
            let key = expect_string_key(key, "context")?;
            match key.as_str() {
                "ModuleRoot" => {
                    options.module_root = Some(match value {
                        LuaValue::String(path) => PathBuf::from(path.to_str()?.to_owned()),
                        other => {
                            return Err(LuaError::runtime(format!(
                                "ModuleRoot must be a string, got {}",
                                other.type_name()
                            )));
                        }
                    });
                }
                "MemoryLimitBytes" => {
                    options.memory_limit_bytes = Some(nonnegative_integer(value, &key)?);
                }
                "StackLimitBytes" => {
                    options.stack_limit_bytes = Some(nonnegative_integer(value, &key)?);
                }
                "ExecutionLimitMs" => {
                    options.execution_limit = Some(positive_duration(value, &key)?);
                }
                "OperationTimeoutMs" => {
                    options.operation_timeout = Some(positive_duration(value, &key)?);
                }
                "JobBudget" => {
                    let budget = nonnegative_integer(value, &key)?;
                    if budget == 0 {
                        return Err(LuaError::runtime("JobBudget must be greater than zero"));
                    }
                    options.job_budget = budget;
                }
                _ => {
                    return Err(LuaError::runtime(format!(
                        "Unknown JavaScript context option \"{key}\""
                    )));
                }
            }
        }
        Ok(options)
    }
}

impl FromLua for JsEvaluateOptions {
    fn from_lua(value: LuaValue, _: &Lua) -> LuaResult<Self> {
        let LuaValue::Table(table) = value else {
            return if value == LuaValue::Nil {
                Ok(Self::default())
            } else {
                Err(LuaError::FromLuaConversionError {
                    from: value.type_name(),
                    to: "JsEvaluateOptions".to_string(),
                    message: Some(format!(
                        "Invalid evaluation options; expected table, got {}",
                        value.type_name()
                    )),
                })
            };
        };

        let mut options = Self::default();
        for pair in table.pairs::<LuaValue, LuaValue>() {
            let (key, value) = pair?;
            let key = expect_string_key(key, "evaluation")?;
            match key.as_str() {
                "Filename" => {
                    options.filename = Some(match value {
                        LuaValue::String(filename) => filename.to_str()?.to_owned(),
                        other => {
                            return Err(LuaError::runtime(format!(
                                "Filename must be a string, got {}",
                                other.type_name()
                            )));
                        }
                    });
                }
                "Mode" => {
                    options.module = match value {
                        LuaValue::String(mode) if mode == "script" => false,
                        LuaValue::String(mode) if mode == "module" => true,
                        LuaValue::String(mode) => {
                            return Err(LuaError::runtime(format!(
                                "Mode must be \"script\" or \"module\", got \"{}\"",
                                mode.to_string_lossy()
                            )));
                        }
                        other => {
                            return Err(LuaError::runtime(format!(
                                "Mode must be a string, got {}",
                                other.type_name()
                            )));
                        }
                    };
                }
                _ => {
                    return Err(LuaError::runtime(format!(
                        "Unknown JavaScript evaluation option \"{key}\""
                    )));
                }
            }
        }
        Ok(options)
    }
}

fn expect_string_key(value: LuaValue, kind: &str) -> LuaResult<String> {
    match value {
        LuaValue::String(key) => Ok(key.to_str()?.to_owned()),
        other => Err(LuaError::runtime(format!(
            "JavaScript {kind} option names must be strings, got {}",
            other.type_name()
        ))),
    }
}

fn nonnegative_integer(value: LuaValue, name: &str) -> LuaResult<usize> {
    let number = match value {
        LuaValue::Integer(number) if number >= 0 => number as usize,
        LuaValue::Number(number)
            if number.is_finite()
                && number >= 0.0
                && number.fract() == 0.0
                && number <= usize::MAX as f64 =>
        {
            number as usize
        }
        other => {
            return Err(LuaError::runtime(format!(
                "{name} must be a nonnegative integer, got {}",
                other.type_name()
            )));
        }
    };
    Ok(number)
}

fn positive_duration(value: LuaValue, name: &str) -> LuaResult<Duration> {
    let milliseconds = nonnegative_integer(value, name)?;
    if milliseconds == 0 {
        return Err(LuaError::runtime(format!(
            "{name} must be greater than zero"
        )));
    }
    Ok(Duration::from_millis(milliseconds as u64))
}
