//! The `grep` tool, ported from `core/tools/grep.ts`.
//!
//! The TypeScript tool shells out to ripgrep. This port searches in process with
//! the same libraries (`ignore`, `regex`), so it needs no external binary, and
//! reproduces the ripgrep behaviors the tool relied on: `.gitignore` is honored
//! inside git repositories, hidden files are searched, binary files are skipped,
//! and `--glob` filters by pattern.
//!
//! Differences: the `.git` directory is never searched (ripgrep with `--hidden`
//! would search it); files are visited in sorted path order (ripgrep's order
//! depends on its threads); the regex dialect is the Rust `regex` crate's, which is also
//! ripgrep's default; a line longer than 500 characters is cut by Unicode scalar
//! values, where the TypeScript tool counts UTF-16 units.

use std::path::Path;

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use nyan_coding_agent_core::ExecutionContext;
use regex::RegexBuilder;
use serde_json::Value;

use super::paths::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, format_size, truncate_head};
use super::{SyncTool, optional_number, optional_string, required_string};
use crate::ToolError;

const DEFAULT_LIMIT: usize = 100;
const GREP_MAX_LINE_LENGTH: usize = 500;
/// Bytes inspected for a NUL to decide whether a file is binary, as ripgrep does.
const BINARY_SNIFF_BYTES: usize = 8192;

const SCHEMA: &str = r#"{"type":"object","properties":{"pattern":{"type":"string","description":"Search pattern (regex or literal string)"},"path":{"type":"string","description":"Directory or file to search (default: current directory)"},"glob":{"type":"string","description":"Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"},"ignoreCase":{"type":"boolean","description":"Case-insensitive search (default: false)"},"literal":{"type":"boolean","description":"Treat pattern as literal string instead of regex (default: false)"},"context":{"type":"number","description":"Number of lines to show before and after each match (default: 0)"},"limit":{"type":"number","description":"Maximum number of matches to return (default: 100)"}},"required":["pattern"]}"#;

pub(super) fn tool() -> SyncTool {
    SyncTool {
        name: "grep",
        description: format!(
            "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} matches or {}KB (whichever is hit first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
            DEFAULT_MAX_BYTES / 1024
        ),
        schema: SCHEMA,
        run,
    }
}

fn optional_bool(arguments: &Value, name: &str) -> Result<bool, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::InvalidInput {
            message: format!("Argument \"{name}\" must be a boolean"),
        }),
    }
}

/// Cuts a line to the display limit, returning whether it was cut.
fn truncate_line(line: &str) -> (String, bool) {
    if line.chars().count() <= GREP_MAX_LINE_LENGTH {
        return (line.to_owned(), false);
    }
    let kept: String = line.chars().take(GREP_MAX_LINE_LENGTH).collect();
    (format!("{kept}... [truncated]"), true)
}

fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn display_path(file: &Path, search: &Path, is_directory: bool) -> String {
    if is_directory
        && let Ok(relative) = file.strip_prefix(search)
        && !relative.as_os_str().is_empty()
    {
        return relative.to_string_lossy().replace('\\', "/");
    }
    file.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// The files to search, in sorted path order.
fn collect_files(
    search: &Path,
    is_directory: bool,
    glob: Option<&str>,
) -> Result<Vec<std::path::PathBuf>, ToolError> {
    if !is_directory {
        return Ok(vec![search.to_path_buf()]);
    }
    let mut builder = WalkBuilder::new(search);
    builder
        .hidden(false)
        .sort_by_file_path(Ord::cmp)
        .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git");
    if let Some(glob) = glob {
        let overrides = OverrideBuilder::new(search)
            .add(glob)
            .and_then(|builder| builder.build())
            .map_err(|error| ToolError::Execution {
                message: format!("Invalid glob \"{glob}\": {error}"),
            })?;
        builder.overrides(overrides);
    }
    Ok(builder
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .collect())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn run(arguments: &Value, context: &ExecutionContext) -> Result<String, ToolError> {
    let pattern = required_string(arguments, "pattern")?;
    let search_dir = optional_string(arguments, "path")?;
    let glob = optional_string(arguments, "glob")?;
    let ignore_case = optional_bool(arguments, "ignoreCase")?;
    let literal = optional_bool(arguments, "literal")?;
    let context_lines = optional_number(arguments, "context")?
        .filter(|value| *value > 0.0)
        .map_or(0, |value| value as usize);
    let limit = optional_number(arguments, "limit")?
        .map_or(DEFAULT_LIMIT, |value| (value.max(1.0)) as usize);

    let search = resolve_to_cwd(
        search_dir.filter(|path| !path.is_empty()).unwrap_or("."),
        &context.cwd,
    );
    let metadata = std::fs::metadata(&search).map_err(|_| ToolError::Execution {
        message: format!("Path not found: {}", search.display()),
    })?;
    let is_directory = metadata.is_dir();

    let source = if literal {
        regex::escape(pattern)
    } else {
        pattern.to_owned()
    };
    let matcher = RegexBuilder::new(&source)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|error| ToolError::Execution {
            message: error.to_string().trim().to_owned(),
        })?;

    let files = collect_files(&search, is_directory, glob)?;

    let mut output_lines: Vec<String> = Vec::new();
    let mut match_count = 0;
    let mut match_limit_reached = false;
    let mut lines_truncated = false;
    'files: for file in &files {
        if context.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let Some(text) = read_text(file) else {
            continue;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let shown = display_path(file, &search, is_directory);
        for (index, line) in lines.iter().enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if !matcher.is_match(line) {
                continue;
            }
            match_count += 1;
            let number = index + 1;
            let first = if context_lines > 0 {
                number.saturating_sub(context_lines).max(1)
            } else {
                number
            };
            let last = if context_lines > 0 {
                (number + context_lines).min(lines.len())
            } else {
                number
            };
            for current in first..=last {
                let raw = lines.get(current - 1).copied().unwrap_or("");
                let (shown_text, was_truncated) = truncate_line(&raw.replace('\r', ""));
                lines_truncated |= was_truncated;
                if current == number {
                    output_lines.push(format!("{shown}:{current}: {shown_text}"));
                } else {
                    output_lines.push(format!("{shown}-{current}- {shown_text}"));
                }
            }
            if match_count >= limit {
                match_limit_reached = true;
                break 'files;
            }
        }
    }
    if match_count == 0 {
        return Ok("No matches found".to_owned());
    }

    let truncation = truncate_head(&output_lines.join("\n"), usize::MAX, DEFAULT_MAX_BYTES);
    let mut output = truncation.content;
    let mut notices = Vec::new();
    if match_limit_reached {
        notices.push(format!(
            "{limit} matches limit reached. Use limit={} for more, or refine pattern",
            limit * 2
        ));
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if lines_truncated {
        notices.push(format!(
            "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
        ));
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

    fn fixture(name: &str) -> TempDir {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(dir.path().join("src/deep")).expect("mkdir");
        std::fs::write(
            dir.path().join("src/a.rs"),
            "fn alpha() {}\nlet Beta = 1;\n",
        )
        .expect("write");
        std::fs::write(
            dir.path().join("src/deep/b.rs"),
            "// alpha again\nnothing\n",
        )
        .expect("write");
        std::fs::write(dir.path().join("notes.txt"), "Alpha in notes\n").expect("write");
        dir
    }

    #[test]
    fn matches_carry_a_relative_path_and_line_number() {
        let dir = fixture("grep-basic");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "alpha"})).expect("grep"),
            "src/a.rs:1: fn alpha() {}\nsrc/deep/b.rs:1: // alpha again"
        );
    }

    #[test]
    fn ignore_case_and_literal_change_how_the_pattern_matches() {
        let dir = fixture("grep-flags");
        let insensitive = call(
            run,
            dir.path(),
            &json!({"pattern": "alpha", "ignoreCase": true}),
        )
        .expect("grep");
        assert!(
            insensitive.contains("notes.txt:1: Alpha in notes"),
            "{insensitive}"
        );
        std::fs::write(dir.path().join("dots.txt"), "a.c\nabc\n").expect("write");
        assert_eq!(
            call(
                run,
                dir.path(),
                &json!({"pattern": "a.c", "path": "dots.txt"})
            )
            .expect("regex"),
            "dots.txt:1: a.c\ndots.txt:2: abc"
        );
        assert_eq!(
            call(
                run,
                dir.path(),
                &json!({"pattern": "a.c", "path": "dots.txt", "literal": true})
            )
            .expect("literal"),
            "dots.txt:1: a.c"
        );
    }

    #[test]
    fn glob_filters_files() {
        let dir = fixture("grep-glob");
        assert_eq!(
            call(
                run,
                dir.path(),
                &json!({"pattern": "alpha", "glob": "*.rs"})
            )
            .expect("grep"),
            "src/a.rs:1: fn alpha() {}\nsrc/deep/b.rs:1: // alpha again"
        );
        assert_eq!(
            call(
                run,
                dir.path(),
                &json!({"pattern": "alpha", "ignoreCase": true, "glob": "*.txt"})
            )
            .expect("grep"),
            "notes.txt:1: Alpha in notes"
        );
    }

    #[test]
    fn context_lines_use_a_dash_marker_and_the_match_uses_a_colon() {
        let dir = TempDir::new("grep-context");
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\nTARGET\nfour\nfive\n").expect("write");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "TARGET", "context": 1})).expect("grep"),
            "f.txt-2- two\nf.txt:3: TARGET\nf.txt-4- four"
        );
    }

    #[test]
    fn the_match_limit_stops_the_search_and_says_how_to_raise_it() {
        let dir = TempDir::new("grep-limit");
        std::fs::write(dir.path().join("f.txt"), "x\nx\nx\nx\n").expect("write");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "x", "limit": 2})).expect("grep"),
            "f.txt:1: x\nf.txt:2: x\n\n[2 matches limit reached. Use limit=4 for more, or refine pattern]"
        );
    }

    #[test]
    fn long_lines_are_cut_with_a_notice() {
        let dir = TempDir::new("grep-long");
        std::fs::write(
            dir.path().join("f.txt"),
            format!("needle{}\n", "z".repeat(600)),
        )
        .expect("write");
        let output = call(run, dir.path(), &json!({"pattern": "needle"})).expect("grep");
        assert!(output.contains("... [truncated]"));
        assert!(
            output
                .ends_with("[Some lines truncated to 500 chars. Use read tool to see full lines]")
        );
    }

    #[test]
    fn gitignored_and_binary_files_are_skipped_but_hidden_files_are_searched() {
        let dir = TempDir::new("grep-ignore");
        std::fs::create_dir(dir.path().join(".git")).expect("git dir");
        std::fs::write(dir.path().join(".gitignore"), "skipped.txt\n").expect("write");
        std::fs::write(dir.path().join("skipped.txt"), "needle\n").expect("write");
        std::fs::write(dir.path().join(".hidden.txt"), "needle\n").expect("write");
        std::fs::write(dir.path().join("bin.dat"), b"needle\0binary").expect("write");
        std::fs::write(dir.path().join(".git/config"), "needle\n").expect("write");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "needle"})).expect("grep"),
            ".hidden.txt:1: needle"
        );
    }

    #[test]
    fn no_match_a_bad_path_and_a_bad_pattern_are_reported() {
        let dir = fixture("grep-errors");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "zzzz"})).expect("grep"),
            "No matches found"
        );
        let missing =
            call(run, dir.path(), &json!({"pattern": "a", "path": "nope"})).expect_err("path");
        assert!(missing.to_string().starts_with("Path not found: "));
        let bad = call(run, dir.path(), &json!({"pattern": "("})).expect_err("regex");
        assert!(bad.to_string().contains("regex parse error"), "{bad}");
    }

    #[test]
    fn a_single_file_search_shows_the_file_name_only() {
        let dir = fixture("grep-file");
        assert_eq!(
            call(
                run,
                dir.path(),
                &json!({"pattern": "Beta", "path": "src/a.rs"})
            )
            .expect("grep"),
            "a.rs:2: let Beta = 1;"
        );
    }
}
