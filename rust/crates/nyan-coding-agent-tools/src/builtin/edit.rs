//! The `edit` tool, ported from `core/tools/edit.ts` and `edit-diff.ts`.
//!
//! Every `oldText` is matched against the original file, never against the
//! result of an earlier edit, and must match exactly once. CRLF files keep their
//! line endings and a leading byte-order mark is preserved.
//!
//! Parity gap: the TypeScript tool falls back to a fuzzy match (smart quotes,
//! Unicode dashes and spaces, trailing whitespace) when the exact text is not
//! found. This port matches exactly only, so such an edit fails with the same
//! "Could not find" error instead of succeeding. It also reports no diff details.

use nyan_coding_agent_core::ExecutionContext;
use serde_json::Value;

use super::paths::resolve_to_cwd;
use super::{SyncTool, required_string};
use crate::ToolError;

const SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Path to the file to edit (relative or absolute)"},"edits":{"type":"array","description":"One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.","items":{"type":"object","properties":{"oldText":{"type":"string","description":"Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."},"newText":{"type":"string","description":"Replacement text for this targeted edit."}},"required":["oldText","newText"]}}},"required":["path","edits"]}"#;

pub(super) fn tool() -> SyncTool {
    SyncTool {
        name: "edit",
        description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_owned(),
        schema: SCHEMA,
        run,
    }
}

struct Edit {
    old_text: String,
    new_text: String,
}

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn detect_line_ending(content: &str) -> &'static str {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

fn read_edits(arguments: &Value) -> Result<Vec<Edit>, ToolError> {
    let mut edits = Vec::new();
    if let Some(Value::Array(items)) = arguments.get("edits") {
        for (index, item) in items.iter().enumerate() {
            let text = |name: &str| {
                item.get(name)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| ToolError::InvalidInput {
                        message: format!("edits[{index}].{name} must be a string"),
                    })
            };
            edits.push(Edit {
                old_text: text("oldText")?,
                new_text: text("newText")?,
            });
        }
    }
    // Older models send a single replacement at the top level.
    if let (Some(Value::String(old)), Some(Value::String(new))) =
        (arguments.get("oldText"), arguments.get("newText"))
    {
        edits.push(Edit {
            old_text: old.clone(),
            new_text: new.clone(),
        });
    }
    if edits.is_empty() {
        return Err(ToolError::InvalidInput {
            message: "Edit tool input is invalid. edits must contain at least one replacement."
                .to_owned(),
        });
    }
    Ok(edits)
}

fn fail(message: String) -> ToolError {
    ToolError::Execution { message }
}

/// Applies the edits to LF-normalized content.
fn apply_edits(content: &str, edits: &[Edit], path: &str) -> Result<String, ToolError> {
    let total = edits.len();
    let mut matched: Vec<(usize, usize, usize, String)> = Vec::with_capacity(total);
    for (index, edit) in edits.iter().enumerate() {
        let old_text = normalize_to_lf(&edit.old_text);
        if old_text.is_empty() {
            return Err(fail(if total == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{index}].oldText must not be empty in {path}.")
            }));
        }
        let Some(position) = content.find(&old_text) else {
            return Err(fail(if total == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                )
            } else {
                format!(
                    "Could not find edits[{index}] in {path}. The oldText must match exactly including all whitespace and newlines."
                )
            }));
        };
        let occurrences = content.matches(&old_text).count();
        if occurrences > 1 {
            return Err(fail(if total == 1 {
                format!(
                    "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
                )
            } else {
                format!(
                    "Found {occurrences} occurrences of edits[{index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
                )
            }));
        }
        matched.push((
            position,
            old_text.len(),
            index,
            normalize_to_lf(&edit.new_text),
        ));
    }
    matched.sort_by_key(|(position, ..)| *position);
    for pair in matched.windows(2) {
        let (previous_position, previous_length, previous_index, _) = &pair[0];
        let (position, _, index, _) = &pair[1];
        if previous_position + previous_length > *position {
            return Err(fail(format!(
                "edits[{previous_index}] and edits[{index}] overlap in {path}. Merge them into one edit or target disjoint regions."
            )));
        }
    }
    let mut result = String::with_capacity(content.len());
    let mut cursor = 0;
    for (position, length, _, new_text) in &matched {
        result.push_str(&content[cursor..*position]);
        result.push_str(new_text);
        cursor = position + length;
    }
    result.push_str(&content[cursor..]);
    if result == content {
        return Err(fail(if total == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            )
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        }));
    }
    Ok(result)
}

fn run(arguments: &Value, context: &ExecutionContext) -> Result<String, ToolError> {
    let path = required_string(arguments, "path")?;
    let edits = read_edits(arguments)?;
    let absolute = resolve_to_cwd(path, &context.cwd);

    let bytes = std::fs::read(&absolute).map_err(|error| {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => "ENOENT".to_owned(),
            std::io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
            _ => error.to_string(),
        };
        let detail = if code.starts_with('E') && code.chars().all(|c| c.is_ascii_uppercase()) {
            format!("Error code: {code}")
        } else {
            code
        };
        fail(format!("Could not edit file: {path}. {detail}."))
    })?;
    let raw = String::from_utf8_lossy(&bytes);
    let (bom, body) = raw
        .strip_prefix('\u{feff}')
        .map_or(("", raw.as_ref()), |rest| ("\u{feff}", rest));
    let ending = detect_line_ending(body);
    let normalized = normalize_to_lf(body);
    let updated = apply_edits(&normalized, &edits, path)?;
    let restored = if ending == "\r\n" {
        updated.replace('\n', "\r\n")
    } else {
        updated
    };
    std::fs::write(&absolute, format!("{bom}{restored}"))
        .map_err(|error| super::io_error(&error, "open", &absolute))?;
    Ok(format!(
        "Successfully replaced {} block(s) in {path}.",
        edits.len()
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::run;
    use crate::builtin::test_support::{TempDir, call};

    fn edit(dir: &TempDir, content: &str, arguments: &Value) -> (Result<String, String>, String) {
        std::fs::write(dir.path().join("f.txt"), content).expect("write fixture");
        let result = call(run, dir.path(), arguments).map_err(|error| error.to_string());
        let after = std::fs::read_to_string(dir.path().join("f.txt")).expect("read back");
        (result, after)
    }

    #[test]
    fn disjoint_edits_apply_against_the_original_text() {
        let dir = TempDir::new("edit-ok");
        let (result, after) = edit(
            &dir,
            "alpha\nbeta\ngamma\n",
            &json!({"path": "f.txt", "edits": [
                {"oldText": "alpha", "newText": "beta"},
                {"oldText": "gamma", "newText": "delta"},
            ]}),
        );
        assert_eq!(
            result.expect("edit"),
            "Successfully replaced 2 block(s) in f.txt."
        );
        assert_eq!(after, "beta\nbeta\ndelta\n");
    }

    #[test]
    fn each_failure_leaves_the_file_untouched_and_names_the_edit() {
        let dir = TempDir::new("edit-errors");
        let original = "one\ntwo\ntwo\n";
        let cases = [
            (
                json!({"path": "f.txt", "edits": [{"oldText": "missing", "newText": "x"}]}),
                "Could not find the exact text in f.txt. The old text must match exactly including all whitespace and newlines.",
            ),
            (
                json!({"path": "f.txt", "edits": [{"oldText": "two", "newText": "x"}]}),
                "Found 2 occurrences of the text in f.txt. The text must be unique. Please provide more context to make it unique.",
            ),
            (
                json!({"path": "f.txt", "edits": [{"oldText": "", "newText": "x"}]}),
                "oldText must not be empty in f.txt.",
            ),
            (
                json!({"path": "f.txt", "edits": [{"oldText": "one", "newText": "one"}]}),
                "No changes made to f.txt. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.",
            ),
            (
                json!({"path": "f.txt", "edits": [
                    {"oldText": "one", "newText": "1"},
                    {"oldText": "nope", "newText": "x"},
                ]}),
                "Could not find edits[1] in f.txt. The oldText must match exactly including all whitespace and newlines.",
            ),
        ];
        for (arguments, expected) in cases {
            let (result, after) = edit(&dir, original, &arguments);
            let message = result.expect_err("edit must fail");
            assert!(message.starts_with(expected), "{message}");
            assert_eq!(after, original, "a failed edit must not change the file");
        }
    }

    #[test]
    fn overlapping_edits_are_rejected() {
        let dir = TempDir::new("edit-overlap");
        let original = "abcdef";
        let (result, after) = edit(
            &dir,
            original,
            &json!({"path": "f.txt", "edits": [
                {"oldText": "abcd", "newText": "1"},
                {"oldText": "cdef", "newText": "2"},
            ]}),
        );
        assert_eq!(
            result.expect_err("overlap"),
            "edits[0] and edits[1] overlap in f.txt. Merge them into one edit or target disjoint regions."
        );
        assert_eq!(after, original);
    }

    #[test]
    fn crlf_files_keep_their_line_endings_and_a_bom_survives() {
        let dir = TempDir::new("edit-crlf");
        let (result, after) = edit(
            &dir,
            "\u{feff}a\r\nb\r\nc\r\n",
            &json!({"path": "f.txt", "edits": [{"oldText": "b\nc", "newText": "x\ny\nz"}]}),
        );
        result.expect("edit");
        assert_eq!(after, "\u{feff}a\r\nx\r\ny\r\nz\r\n");
    }

    #[test]
    fn the_legacy_single_replacement_form_is_accepted() {
        let dir = TempDir::new("edit-legacy");
        let (result, after) = edit(
            &dir,
            "hello world",
            &json!({"path": "f.txt", "oldText": "world", "newText": "there"}),
        );
        assert_eq!(
            result.expect("edit"),
            "Successfully replaced 1 block(s) in f.txt."
        );
        assert_eq!(after, "hello there");
    }

    #[test]
    fn empty_edits_and_a_missing_file_are_reported() {
        let dir = TempDir::new("edit-input");
        let empty =
            call(run, dir.path(), &json!({"path": "f.txt", "edits": []})).expect_err("empty");
        assert_eq!(
            empty.to_string(),
            "Edit tool input is invalid. edits must contain at least one replacement."
        );
        let missing = call(
            run,
            dir.path(),
            &json!({"path": "nope.txt", "edits": [{"oldText": "a", "newText": "b"}]}),
        )
        .expect_err("missing file");
        assert_eq!(
            missing.to_string(),
            "Could not edit file: nope.txt. Error code: ENOENT."
        );
    }
}
