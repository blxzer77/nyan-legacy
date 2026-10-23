//! The `read` tool, ported from `core/tools/read.ts` (text files).
//!
//! Parity gap: image files are refused with an explanation instead of being
//! resized and attached, because the Rust runtime has no image content block yet.

use nyan_coding_agent_core::ExecutionContext;
use serde_json::Value;

use super::paths::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_head};
use super::{SyncTool, io_error, optional_number, required_string};
use crate::ToolError;

const SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Path to the file to read (relative or absolute)"},"offset":{"type":"number","description":"Line number to start reading from (1-indexed)"},"limit":{"type":"number","description":"Maximum number of lines to read"}},"required":["path"]}"#;

pub(super) fn tool() -> SyncTool {
    SyncTool {
        name: "read",
        description: format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_BYTES / 1024
        ),
        schema: SCHEMA,
        run,
    }
}

fn is_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") && bytes.len() > 14 && bytes[6..10] == [0, 0, 0, 0] {
        Some("image/bmp")
    } else {
        None
    }
}

/// JavaScript truthiness of a number: 0 and NaN are false.
fn truthy(value: f64) -> bool {
    value != 0.0 && !value.is_nan()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn run(arguments: &Value, context: &ExecutionContext) -> Result<String, ToolError> {
    let path = required_string(arguments, "path")?;
    let offset = optional_number(arguments, "offset")?;
    let limit = optional_number(arguments, "limit")?;
    let absolute = resolve_to_cwd(path, &context.cwd);
    let bytes = std::fs::read(&absolute).map_err(|error| io_error(&error, "open", &absolute))?;
    if let Some(mime) = is_image(&bytes) {
        return Err(ToolError::Execution {
            message: format!(
                "{path} is an image ({mime}). The Rust preview cannot attach images yet; use a text tool or the TypeScript nyan command."
            ),
        });
    }
    let text = String::from_utf8_lossy(&bytes);
    let all_lines: Vec<&str> = text.split('\n').collect();
    let total_file_lines = all_lines.len();

    let start_line = match offset {
        Some(offset) if truthy(offset) => (offset - 1.0).max(0.0) as usize,
        _ => 0,
    };
    let start_line_display = start_line + 1;
    if start_line >= total_file_lines {
        return Err(ToolError::Execution {
            message: format!(
                "Offset {} is beyond end of file ({total_file_lines} lines total)",
                offset.unwrap_or(0.0)
            ),
        });
    }

    let (selected, user_limited_lines) = match limit {
        Some(limit) => {
            let end = start_line
                .saturating_add(limit.max(0.0) as usize)
                .min(total_file_lines);
            (
                all_lines[start_line..end].join("\n"),
                Some(end - start_line),
            )
        }
        None => (all_lines[start_line..].join("\n"), None),
    };
    let truncation = truncate_head(&selected, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);

    if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len());
        return Ok(format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES)
        ));
    }
    if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines - 1;
        let next_offset = end_line_display + 1;
        let mut output = truncation.content;
        if truncation.truncated_by == Some(super::truncate::TruncatedBy::Lines) {
            output = format!(
                "{output}\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            );
        } else {
            output = format!(
                "{output}\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            );
        }
        return Ok(output);
    }
    if let Some(user_limited) = user_limited_lines
        && start_line + user_limited < total_file_lines
    {
        let remaining = total_file_lines - (start_line + user_limited);
        let next_offset = start_line + user_limited + 1;
        return Ok(format!(
            "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
            truncation.content
        ));
    }
    Ok(truncation.content)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::run;
    use crate::builtin::test_support::{TempDir, call};

    fn numbered(count: usize) -> String {
        (1..=count)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn small_files_are_returned_whole() {
        let dir = TempDir::new("read-small");
        std::fs::write(dir.path().join("a.txt"), "hello\nworld").expect("write");
        assert_eq!(
            call(run, dir.path(), &json!({"path": "a.txt"})).expect("read"),
            "hello\nworld"
        );
    }

    #[test]
    fn the_line_limit_adds_a_continuation_notice() {
        let dir = TempDir::new("read-lines");
        std::fs::write(dir.path().join("big.txt"), numbered(2500)).expect("write");
        let output = call(run, dir.path(), &json!({"path": "big.txt"})).expect("read");
        assert!(output.starts_with("line 1\n"));
        assert!(
            output.contains(
                "line 2000\n\n[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"
            )
        );
        assert!(!output.contains("line 2001\n"));
    }

    #[test]
    fn offset_and_limit_select_a_window_and_report_what_remains() {
        let dir = TempDir::new("read-window");
        std::fs::write(dir.path().join("w.txt"), numbered(10)).expect("write");
        assert_eq!(
            call(
                run,
                dir.path(),
                &json!({"path": "w.txt", "offset": 3, "limit": 2})
            )
            .expect("read"),
            "line 3\nline 4\n\n[6 more lines in file. Use offset=5 to continue.]"
        );
        assert_eq!(
            call(run, dir.path(), &json!({"path": "w.txt", "offset": 9})).expect("read"),
            "line 9\nline 10"
        );
    }

    #[test]
    fn an_offset_past_the_end_is_an_error_that_names_the_length() {
        let dir = TempDir::new("read-offset");
        std::fs::write(dir.path().join("w.txt"), numbered(3)).expect("write");
        let error =
            call(run, dir.path(), &json!({"path": "w.txt", "offset": 4})).expect_err("past end");
        assert_eq!(
            error.to_string(),
            "Offset 4 is beyond end of file (3 lines total)"
        );
    }

    #[test]
    fn a_giant_first_line_points_at_the_bash_fallback() {
        let dir = TempDir::new("read-giant");
        std::fs::write(dir.path().join("g.txt"), "x".repeat(60_000)).expect("write");
        let output = call(run, dir.path(), &json!({"path": "g.txt"})).expect("read");
        assert_eq!(
            output,
            "[Line 1 is 58.6KB, exceeds 50.0KB limit. Use bash: sed -n '1p' g.txt | head -c 51200]"
        );
    }

    #[test]
    fn missing_files_and_bad_arguments_are_reported() {
        let dir = TempDir::new("read-errors");
        let missing = call(run, dir.path(), &json!({"path": "nope.txt"})).expect_err("missing");
        assert!(
            missing
                .to_string()
                .starts_with("ENOENT: no such file or directory, open '")
        );
        let no_path = call(run, dir.path(), &json!({})).expect_err("no path");
        assert_eq!(no_path.to_string(), "Missing required argument \"path\"");
        let wrong = call(run, dir.path(), &json!({"path": 3})).expect_err("wrong type");
        assert_eq!(wrong.to_string(), "Argument \"path\" must be a string");
    }

    #[test]
    fn images_are_refused_rather_than_dumped_as_text() {
        let dir = TempDir::new("read-image");
        std::fs::write(dir.path().join("i.png"), b"\x89PNG\r\n\x1a\nrest").expect("write");
        let error = call(run, dir.path(), &json!({"path": "i.png"})).expect_err("image");
        assert!(error.to_string().contains("is an image (image/png)"));
    }
}
