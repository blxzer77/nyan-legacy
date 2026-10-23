//! The `ls` tool, ported from `core/tools/ls.ts`.
//!
//! Parity note: entries sort case-insensitively by byte order of the lowercased
//! name. The TypeScript tool uses ICU `localeCompare`, which orders punctuation
//! differently, so names that differ only in punctuation can appear in a
//! different order.

use nyan_coding_agent_core::ExecutionContext;
use serde_json::Value;

use super::paths::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, format_size, truncate_head};
use super::{SyncTool, optional_number, optional_string};
use crate::ToolError;

const DEFAULT_LIMIT: usize = 500;

const SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Directory to list (default: current directory)"},"limit":{"type":"number","description":"Maximum number of entries to return (default: 500)"}}}"#;

pub(super) fn tool() -> SyncTool {
    SyncTool {
        name: "ls",
        description: format!(
            "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to {DEFAULT_LIMIT} entries or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        ),
        schema: SCHEMA,
        run,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn run(arguments: &Value, context: &ExecutionContext) -> Result<String, ToolError> {
    let path = optional_string(arguments, "path")?;
    let limit = optional_number(arguments, "limit")?;
    let dir = resolve_to_cwd(
        path.filter(|path| !path.is_empty()).unwrap_or("."),
        &context.cwd,
    );
    let effective_limit = limit.map_or(DEFAULT_LIMIT, |limit| limit.max(0.0) as usize);

    let metadata = std::fs::metadata(&dir).map_err(|_| ToolError::Execution {
        message: format!("Path not found: {}", dir.display()),
    })?;
    if !metadata.is_dir() {
        return Err(ToolError::Execution {
            message: format!("Not a directory: {}", dir.display()),
        });
    }
    let mut entries: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|error| ToolError::Execution {
            message: format!("Cannot read directory: {error}"),
        })?
        .filter_map(|entry| {
            entry
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
        })
        .collect();
    entries.sort_by_key(|name| name.to_lowercase());

    let mut results = Vec::new();
    let mut entry_limit_reached = false;
    for entry in entries {
        if results.len() >= effective_limit {
            entry_limit_reached = true;
            break;
        }
        // Follow symlinks like `stat`; skip entries that cannot be inspected.
        let Ok(metadata) = std::fs::metadata(dir.join(&entry)) else {
            continue;
        };
        results.push(if metadata.is_dir() {
            format!("{entry}/")
        } else {
            entry
        });
    }
    if results.is_empty() {
        return Ok("(empty directory)".to_owned());
    }

    let truncation = truncate_head(&results.join("\n"), usize::MAX, DEFAULT_MAX_BYTES);
    let mut output = truncation.content;
    let mut notices = Vec::new();
    if entry_limit_reached {
        notices.push(format!(
            "{effective_limit} entries limit reached. Use limit={} for more",
            effective_limit * 2
        ));
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if !notices.is_empty() {
        output = format!("{output}\n\n[{}]", notices.join(". "));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::run;
    use crate::builtin::test_support::{TempDir, call};

    #[test]
    fn entries_sort_case_insensitively_and_directories_get_a_slash() {
        let dir = TempDir::new("ls");
        for name in ["beta.txt", "Alpha.txt", ".hidden"] {
            std::fs::write(dir.path().join(name), "").expect("write file");
        }
        std::fs::create_dir(dir.path().join("Zeta")).expect("mkdir");
        std::fs::create_dir(dir.path().join("gamma")).expect("mkdir");
        assert_eq!(
            call(run, dir.path(), &json!({})).expect("ls"),
            ".hidden\nAlpha.txt\nbeta.txt\ngamma/\nZeta/"
        );
    }

    #[test]
    fn the_entry_limit_adds_a_notice_that_doubles_the_limit() {
        let dir = TempDir::new("ls-limit");
        for index in 0..5 {
            std::fs::write(dir.path().join(format!("f{index}")), "").expect("write file");
        }
        assert_eq!(
            call(run, dir.path(), &json!({"limit": 2})).expect("ls"),
            "f0\nf1\n\n[2 entries limit reached. Use limit=4 for more]"
        );
    }

    #[test]
    fn empty_missing_and_non_directory_paths() {
        let dir = TempDir::new("ls-edge");
        std::fs::create_dir(dir.path().join("empty")).expect("mkdir");
        std::fs::write(dir.path().join("file"), "").expect("write file");
        assert_eq!(
            call(run, dir.path(), &json!({"path": "empty"})).expect("ls"),
            "(empty directory)"
        );
        let missing = call(run, dir.path(), &json!({"path": "nope"})).expect_err("missing");
        assert!(missing.to_string().starts_with("Path not found: "));
        let file = call(run, dir.path(), &json!({"path": "file"})).expect_err("file");
        assert!(file.to_string().starts_with("Not a directory: "));
    }
}
