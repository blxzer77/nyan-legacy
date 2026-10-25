//! The `read` tool, ported from `core/tools/read.ts` (text files).
//!
//! Parity gap: image files are refused with an explanation instead of being
//! resized and attached, because the Rust runtime has no image content block yet.

use std::io::Read as _;

use nyan_coding_agent_core::ExecutionContext;
use serde_json::Value;

use super::paths::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size, truncate_head};
use super::{SyncTool, io_error, optional_number, required_string};
use crate::ToolError;

const SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Path to the file to read (relative or absolute)"},"offset":{"type":"number","description":"Line number to start reading from (1-indexed)"},"limit":{"type":"number","description":"Maximum number of lines to read"}},"required":["path"]}"#;

/// Do not read arbitrary devices or unbounded files into the agent process.
/// The output limit is smaller, but this cap also bounds input memory and makes
/// a path that grows while it is being read fail closed.
const MAX_READ_FILE_BYTES: u64 = 16 * 1024 * 1024;

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

fn read_regular_file(path: &str, absolute: &std::path::Path) -> Result<Vec<u8>, ToolError> {
    // Stat the path before opening it: opening a FIFO can block until another
    // process supplies a writer, which is not an acceptable tool behavior.
    let path_metadata =
        std::fs::metadata(absolute).map_err(|error| io_error(&error, "open", absolute))?;
    if !path_metadata.file_type().is_file() {
        return Err(ToolError::Execution {
            message: format!("Cannot read {path}: the path is not a regular file"),
        });
    }
    if path_metadata.len() > MAX_READ_FILE_BYTES {
        return Err(ToolError::Execution {
            message: format!(
                "Cannot read {path}: the file is {} and exceeds the {} read limit",
                format_size(usize::try_from(path_metadata.len()).unwrap_or(usize::MAX)),
                format_size(usize::try_from(MAX_READ_FILE_BYTES).expect("read limit fits usize")),
            ),
        });
    }
    let file = std::fs::File::open(absolute).map_err(|error| io_error(&error, "open", absolute))?;

    let mut bytes = Vec::new();
    file.take(MAX_READ_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(&error, "read", absolute))?;
    if bytes.len() as u64 > MAX_READ_FILE_BYTES {
        return Err(ToolError::Execution {
            message: format!(
                "Cannot read {path}: the file grew beyond the {} read limit",
                format_size(usize::try_from(MAX_READ_FILE_BYTES).expect("read limit fits usize")),
            ),
        });
    }
    Ok(bytes)
}

fn decode_utf16_bom(bytes: &[u8]) -> Option<String> {
    let (little_endian, body) = if bytes.starts_with(&[0xff, 0xfe]) {
        (true, &bytes[2..])
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (false, &bytes[2..])
    } else {
        return None;
    };
    let (pairs, remainder) = body.as_chunks::<2>();
    let mut decoded = char::decode_utf16(pairs.iter().map(|pair| {
        if little_endian {
            u16::from_le_bytes(*pair)
        } else {
            u16::from_be_bytes(*pair)
        }
    }))
    .map(|result| result.unwrap_or('\u{fffd}'))
    .collect::<String>();
    if !remainder.is_empty() {
        decoded.push('\u{fffd}');
    }
    Some(decoded)
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
    let bytes = read_regular_file(path, &absolute)?;
    if let Some(mime) = is_image(&bytes) {
        return Err(ToolError::Execution {
            message: format!(
                "{path} is an image ({mime}). The Rust preview cannot attach images yet; use a text tool or the TypeScript nyan command."
            ),
        });
    }
    let text =
        decode_utf16_bom(&bytes).unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
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
    fn utf16_little_and_big_endian_files_drop_the_bom_and_decode_text() {
        let dir = TempDir::new("read-utf16");
        std::fs::write(
            dir.path().join("le.txt"),
            [0xff, 0xfe, b'h', 0, b'i', 0, b'!', 0],
        )
        .expect("write little endian");
        std::fs::write(
            dir.path().join("be.txt"),
            [0xfe, 0xff, 0, b'h', 0, b'i', 0, b'!'],
        )
        .expect("write big endian");
        assert_eq!(
            call(run, dir.path(), &json!({"path": "le.txt"})).expect("read little endian"),
            "hi!"
        );
        assert_eq!(
            call(run, dir.path(), &json!({"path": "be.txt"})).expect("read big endian"),
            "hi!"
        );
    }

    #[test]
    fn oversized_files_are_rejected_before_their_contents_are_read() {
        let dir = TempDir::new("read-large");
        let path = dir.path().join("large.bin");
        let file = std::fs::File::create(&path).expect("create sparse file");
        file.set_len(super::MAX_READ_FILE_BYTES + 1)
            .expect("make sparse file");
        let error = call(run, dir.path(), &json!({"path": "large.bin"})).expect_err("large");
        assert_eq!(
            error.to_string(),
            "Cannot read large.bin: the file is 16.0MB and exceeds the 16.0MB read limit"
        );
    }

    #[test]
    fn directories_are_rejected_as_non_regular_files() {
        let dir = TempDir::new("read-directory");
        let error = call(run, dir.path(), &json!({"path": "."})).expect_err("directory");
        assert_eq!(
            error.to_string(),
            "Cannot read .: the path is not a regular file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_devices_and_fifos_are_rejected_without_opening_or_blocking() {
        use std::process::Command;

        let dir = TempDir::new("read-special");
        let fifo = dir.path().join("pipe");
        let status = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
        assert!(status.success(), "mkfifo failed: {status}");
        let fifo_error = call(run, dir.path(), &json!({"path": "pipe"})).expect_err("fifo");
        assert_eq!(
            fifo_error.to_string(),
            "Cannot read pipe: the path is not a regular file"
        );

        let device_error =
            call(run, dir.path(), &json!({"path": "/dev/null"})).expect_err("device");
        assert_eq!(
            device_error.to_string(),
            "Cannot read /dev/null: the path is not a regular file"
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
