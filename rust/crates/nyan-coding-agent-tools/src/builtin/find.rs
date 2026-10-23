//! The `find` tool, ported from `core/tools/find.ts`.
//!
//! The TypeScript tool shells out to `fd`. This port walks in process with the
//! libraries `fd` is built on (`ignore`, `globset`), so it needs no external binary.
//!
//! A pattern without `/` matches names; a pattern with `/` matches the path
//! relative to the search directory, with a leading `**/` added when it has none.
//! `.gitignore` is honored inside git repositories, hidden files are listed, and
//! results are sorted. Directories are listed with a trailing `/`.
//!
//! Unverified assumptions: `fd` is believed to skip `.git` directories even with
//! `--hidden`, and this port does the same; `fd`'s own `.fdignore` file is not read.

use std::path::Path;

use globset::GlobBuilder;
use ignore::WalkBuilder;
use nyan_coding_agent_core::ExecutionContext;
use serde_json::Value;

use super::paths::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, format_size, truncate_head};
use super::{SyncTool, optional_number, optional_string, required_string};
use crate::ToolError;

const DEFAULT_LIMIT: usize = 1000;

const SCHEMA: &str = r#"{"type":"object","properties":{"pattern":{"type":"string","description":"Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'"},"path":{"type":"string","description":"Directory to search in (default: current directory)"},"limit":{"type":"number","description":"Maximum number of results (default: 1000)"}},"required":["pattern"]}"#;

pub(super) fn tool() -> SyncTool {
    SyncTool {
        name: "find",
        description: format!(
            "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} results or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        ),
        schema: SCHEMA,
        run,
    }
}

fn inside_git_repository(start: &Path) -> bool {
    start
        .ancestors()
        .any(|directory| directory.join(".git").exists())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn run(arguments: &Value, context: &ExecutionContext) -> Result<String, ToolError> {
    let pattern = required_string(arguments, "pattern")?;
    let search_dir = optional_string(arguments, "path")?;
    let effective_limit =
        optional_number(arguments, "limit")?.map_or(DEFAULT_LIMIT, |value| value.max(0.0) as usize);

    let search = resolve_to_cwd(
        search_dir.filter(|path| !path.is_empty()).unwrap_or("."),
        &context.cwd,
    );
    if !search.exists() {
        return Err(ToolError::Execution {
            message: format!("Path not found: {}", search.display()),
        });
    }

    let matches_path = pattern.contains('/');
    let effective_pattern = if matches_path
        && !pattern.starts_with('/')
        && !pattern.starts_with("**/")
        && pattern != "**"
    {
        format!("**/{pattern}")
    } else {
        pattern.to_owned()
    };
    let matcher = GlobBuilder::new(&effective_pattern)
        .literal_separator(true)
        .build()
        .map_err(|error| ToolError::Execution {
            message: format!("Invalid glob pattern: {error}"),
        })?
        .compile_matcher();

    let mut builder = WalkBuilder::new(&search);
    builder
        .hidden(false)
        .require_git(inside_git_repository(&search))
        .sort_by_file_path(Ord::cmp)
        .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git");

    let mut results: Vec<String> = Vec::new();
    for entry in builder.build().filter_map(Result::ok) {
        if entry.depth() == 0 {
            continue;
        }
        if context.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let Ok(relative) = entry.path().strip_prefix(&search) else {
            continue;
        };
        let posix = relative.to_string_lossy().replace('\\', "/");
        let candidate = if matches_path {
            posix.as_str()
        } else {
            entry.file_name().to_str().unwrap_or("")
        };
        if !matcher.is_match(candidate) {
            continue;
        }
        let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());
        results.push(if is_directory {
            format!("{posix}/")
        } else {
            posix
        });
        if results.len() >= effective_limit {
            break;
        }
    }
    if results.is_empty() {
        return Ok("No files found matching pattern".to_owned());
    }

    let limit_reached = results.len() >= effective_limit;
    let truncation = truncate_head(&results.join("\n"), usize::MAX, DEFAULT_MAX_BYTES);
    let mut output = truncation.content;
    let mut notices = Vec::new();
    if limit_reached {
        notices.push(format!(
            "{effective_limit} results limit reached. Use limit={} for more, or refine pattern",
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

    fn fixture(name: &str) -> TempDir {
        let dir = TempDir::new(name);
        for path in [
            "src/main.rs",
            "src/lib.rs",
            "src/deep/util.spec.ts",
            "docs/guide.md",
            "README.md",
            ".hidden/secret.md",
        ] {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            std::fs::write(full, "").expect("write");
        }
        dir
    }

    #[test]
    fn a_name_pattern_matches_at_any_depth_and_hidden_files_are_listed() {
        let dir = fixture("find-name");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "*.md"})).expect("find"),
            ".hidden/secret.md\nREADME.md\ndocs/guide.md"
        );
    }

    #[test]
    fn a_pattern_with_a_slash_matches_the_relative_path() {
        let dir = fixture("find-path");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "src/**/*.spec.ts"})).expect("find"),
            "src/deep/util.spec.ts"
        );
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "docs/*.md"})).expect("find"),
            "docs/guide.md"
        );
    }

    #[test]
    fn directories_are_listed_with_a_trailing_slash() {
        let dir = fixture("find-dirs");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "docs"})).expect("find"),
            "docs/"
        );
    }

    #[test]
    fn the_search_directory_can_be_chosen_and_results_stay_relative_to_it() {
        let dir = fixture("find-path-arg");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "*.rs", "path": "src"})).expect("find"),
            "lib.rs\nmain.rs"
        );
    }

    #[test]
    fn the_result_limit_adds_a_notice_that_doubles_the_limit() {
        let dir = fixture("find-limit");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "*.rs", "limit": 1})).expect("find"),
            "src/lib.rs\n\n[1 results limit reached. Use limit=2 for more, or refine pattern]"
        );
    }

    #[test]
    fn gitignored_paths_and_the_git_directory_are_skipped_inside_a_repository() {
        let dir = TempDir::new("find-git");
        std::fs::create_dir(dir.path().join(".git")).expect("git dir");
        std::fs::write(dir.path().join(".git/HEAD"), "").expect("write");
        std::fs::write(dir.path().join(".gitignore"), "build/\n").expect("write");
        std::fs::create_dir(dir.path().join("build")).expect("mkdir");
        std::fs::write(dir.path().join("build/out.txt"), "").expect("write");
        std::fs::write(dir.path().join("keep.txt"), "").expect("write");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "*.txt"})).expect("find"),
            "keep.txt"
        );
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "HEAD"})).expect("find"),
            "No files found matching pattern"
        );
    }

    #[test]
    fn no_match_a_missing_path_and_a_bad_glob_are_reported() {
        let dir = fixture("find-errors");
        assert_eq!(
            call(run, dir.path(), &json!({"pattern": "*.zzz"})).expect("find"),
            "No files found matching pattern"
        );
        let missing =
            call(run, dir.path(), &json!({"pattern": "*", "path": "nope"})).expect_err("path");
        assert!(missing.to_string().starts_with("Path not found: "));
        let bad = call(run, dir.path(), &json!({"pattern": "[abc"})).expect_err("glob");
        assert!(
            bad.to_string().starts_with("Invalid glob pattern: "),
            "{bad}"
        );
    }
}
