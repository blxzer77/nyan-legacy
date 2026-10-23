//! The `write` tool, ported from `core/tools/write.ts`.

use nyan_coding_agent_core::ExecutionContext;
use serde_json::Value;

use super::paths::resolve_to_cwd;
use super::{SyncTool, io_error, required_string};
use crate::ToolError;

const SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Path to the file to write (relative or absolute)"},"content":{"type":"string","description":"Content to write to the file"}},"required":["path","content"]}"#;

pub(super) fn tool() -> SyncTool {
    SyncTool {
        name: "write",
        description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_owned(),
        schema: SCHEMA,
        run,
    }
}

fn run(arguments: &Value, context: &ExecutionContext) -> Result<String, ToolError> {
    let path = required_string(arguments, "path")?;
    let data = required_string(arguments, "content")?;
    let absolute = resolve_to_cwd(path, &context.cwd);
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error(&error, "mkdir", parent))?;
    }
    std::fs::write(&absolute, data).map_err(|error| io_error(&error, "open", &absolute))?;
    Ok(format!("Successfully wrote to {path}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::run;
    use crate::builtin::test_support::{TempDir, call};

    #[test]
    fn writes_create_parent_directories_and_overwrite() {
        let dir = TempDir::new("write");
        let message = call(
            run,
            dir.path(),
            &json!({"path": "deep/er/file.txt", "content": "one"}),
        )
        .expect("write");
        assert_eq!(message, "Successfully wrote to deep/er/file.txt");
        let target = dir.path().join("deep/er/file.txt");
        assert_eq!(std::fs::read_to_string(&target).expect("read back"), "one");
        call(
            run,
            dir.path(),
            &json!({"path": "deep/er/file.txt", "content": "two"}),
        )
        .expect("rewrite");
        assert_eq!(std::fs::read_to_string(&target).expect("read back"), "two");
    }

    #[test]
    fn content_is_written_byte_for_byte() {
        let dir = TempDir::new("write-exact");
        let content = "crlf\r\nunicode \u{2028} \u{4e16}\u{754c}\n";
        call(run, dir.path(), &json!({"path": "x", "content": content})).expect("write");
        assert_eq!(
            std::fs::read(dir.path().join("x")).expect("read back"),
            content.as_bytes()
        );
    }

    #[test]
    fn a_path_that_cannot_be_created_is_reported_and_leaves_no_file() {
        let dir = TempDir::new("write-blocked");
        std::fs::write(dir.path().join("file"), "x").expect("write blocker");
        let error = call(
            run,
            dir.path(),
            &json!({"path": "file/child", "content": "y"}),
        )
        .expect_err("blocked");
        assert_ne!(error.to_string(), "");
        assert!(!dir.path().join("file/child").exists());
    }

    #[test]
    fn missing_content_is_an_invalid_input() {
        let dir = TempDir::new("write-args");
        let error = call(run, dir.path(), &json!({"path": "x"})).expect_err("no content");
        assert_eq!(error.to_string(), "Missing required argument \"content\"");
    }
}
