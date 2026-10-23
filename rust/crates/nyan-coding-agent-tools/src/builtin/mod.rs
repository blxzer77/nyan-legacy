//! The built-in coding tools.
//!
//! Each tool takes its arguments as JSON text, the way a provider delivers them,
//! and reports failures as [`ToolError`] so the runtime shows the model the
//! message. File tools run their blocking work inline; they are short, and the
//! runtime serializes polling of one thread anyway.

mod bash;
mod edit;
mod find;
mod grep;
mod ls;
pub mod paths;
mod read;
pub mod truncate;
mod write;

use nyan_coding_agent_core::{BoxFuture, ExecutionContext, ToolResult};
use serde_json::Value;

use crate::{Tool, ToolDefinition, ToolError, ToolRegistry};

type SyncRun = fn(&Value, &ExecutionContext) -> Result<String, ToolError>;

/// A tool whose work is a plain blocking function of its parsed arguments.
struct SyncTool {
    name: &'static str,
    description: String,
    schema: &'static str,
    run: SyncRun,
}

impl Tool for SyncTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.to_owned(),
            description: self.description.clone(),
            parameters_schema: self.schema.to_owned(),
        }
    }

    fn execute(
        &self,
        input: String,
        context: ExecutionContext,
    ) -> BoxFuture<Result<ToolResult, ToolError>> {
        let run = self.run;
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let arguments = parse_arguments(&input)?;
            run(&arguments, &context).map(|text| ToolResult {
                text,
                is_error: false,
            })
        })
    }
}

/// Parses tool-call arguments. An empty string is an empty object, as providers
/// send for tools that take no arguments.
pub(crate) fn parse_arguments(input: &str) -> Result<Value, ToolError> {
    if input.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    match serde_json::from_str::<Value>(input) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err(ToolError::InvalidInput {
            message: "Tool arguments must be a JSON object".to_owned(),
        }),
        Err(error) => Err(ToolError::InvalidInput {
            message: format!("Tool arguments are not valid JSON: {error}"),
        }),
    }
}

pub(crate) fn required_string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    match arguments.get(name) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(ToolError::InvalidInput {
            message: format!("Argument \"{name}\" must be a string"),
        }),
        None => Err(ToolError::InvalidInput {
            message: format!("Missing required argument \"{name}\""),
        }),
    }
}

pub(crate) fn optional_string<'a>(
    arguments: &'a Value,
    name: &str,
) -> Result<Option<&'a str>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(ToolError::InvalidInput {
            message: format!("Argument \"{name}\" must be a string"),
        }),
    }
}

pub(crate) fn optional_number(arguments: &Value, name: &str) -> Result<Option<f64>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidInput {
                message: format!("Argument \"{name}\" must be a number"),
            }),
    }
}

/// Describes an I/O failure the way Node reports it, so the model sees familiar errors.
pub(crate) fn io_error(error: &std::io::Error, syscall: &str, path: &std::path::Path) -> ToolError {
    let (code, text) = match error.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        std::io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        _ => {
            return ToolError::Execution {
                message: format!("{syscall} '{}': {error}", path.display()),
            };
        }
    };
    ToolError::Execution {
        message: format!("{code}: {text}, {syscall} '{}'", path.display()),
    }
}

/// The built-in tools available to one agent runtime, in declaration order.
pub struct BuiltinTools {
    tools: Vec<Box<dyn Tool>>,
}

impl std::fmt::Debug for BuiltinTools {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_list()
            .entries(self.tools.iter().map(|tool| tool.definition().name))
            .finish()
    }
}

impl BuiltinTools {
    /// The default coding tools, in the TypeScript order: `read`, `bash`, `edit`, `write`.
    #[must_use]
    pub fn coding_tools() -> Self {
        Self {
            tools: vec![
                Box::new(read::tool()),
                Box::new(bash::BashTool),
                Box::new(edit::tool()),
                Box::new(write::tool()),
            ],
        }
    }

    /// The named built-in tools, in declaration order. Unknown names are ignored.
    #[must_use]
    pub fn only(names: &[&str]) -> Self {
        let mut all = Self::all();
        all.tools
            .retain(|tool| names.contains(&tool.definition().name.as_str()));
        all
    }

    /// Every built-in tool: the coding tools plus `grep`, `find`, and `ls`.
    #[must_use]
    pub fn all() -> Self {
        Self {
            tools: vec![
                Box::new(read::tool()),
                Box::new(bash::BashTool),
                Box::new(edit::tool()),
                Box::new(write::tool()),
                Box::new(grep::tool()),
                Box::new(find::tool()),
                Box::new(ls::tool()),
            ],
        }
    }
}

impl ToolRegistry for BuiltinTools {
    fn tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.definition().name == name)
            .map(AsRef::as_ref)
    }

    fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use nyan_coding_agent_core::{CancellationToken, ExecutionContext};
    use serde_json::Value;

    use super::{SyncRun, parse_arguments};
    use crate::ToolError;

    pub(crate) struct TempDir(pub(crate) PathBuf);

    impl TempDir {
        pub(crate) fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("nyan-tools-{name}-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn call(run: SyncRun, cwd: &Path, arguments: &Value) -> Result<String, ToolError> {
        let context = ExecutionContext::new(cwd, CancellationToken::new());
        run(&parse_arguments(&arguments.to_string())?, &context)
    }
}
