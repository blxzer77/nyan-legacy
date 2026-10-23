//! The first prompt of a headless run: piped stdin, `@file` arguments, and the
//! first positional message, ported from `buildInitialMessage` in
//! `cli/initial-message.ts` and `processFileArguments` in `cli/file-processor.ts`.
//!
//! The three parts are concatenated with no separator, exactly as the TypeScript
//! CLI does: trimmed stdin, then the `<file>` blocks (each ends in a newline),
//! then the message text.
//!
//! Image files are not attachments here: the Rust runtime has no image content
//! blocks, so an image `@file` is refused instead of being silently dropped.

use std::fmt::Write as _;
use std::path::Path;

use nyan_coding_agent_tools::builtin::paths::resolve_to_cwd;

fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

/// Renders the `@file` arguments as `<file name="...">` blocks.
///
/// Empty files are skipped.
///
/// # Errors
///
/// Returns a user-facing message for a missing or unreadable file, or an image.
pub fn read_file_arguments(file_args: &[String], cwd: &Path) -> Result<String, String> {
    let mut text = String::new();
    for argument in file_args {
        let path = resolve_to_cwd(argument, cwd);
        let shown = path.display();
        let metadata = std::fs::metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!("File not found: {shown}")
            } else {
                format!("Could not read file {shown}: {error}")
            }
        })?;
        if metadata.len() == 0 {
            continue;
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("Could not read file {shown}: {error}"))?;
        if is_image(&bytes) {
            return Err(format!(
                "{shown} is an image; the Rust preview cannot attach images yet"
            ));
        }
        let content = String::from_utf8_lossy(&bytes);
        let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
        let _ = writeln!(text, "<file name=\"{shown}\">\n{content}\n</file>");
    }
    Ok(text)
}

/// Combines stdin, file text, and the first message; the remaining messages follow.
///
/// Returns the prompts to run in order.
#[must_use]
pub fn build_prompts(stdin: Option<&str>, file_text: &str, messages: &[String]) -> Vec<String> {
    let mut first = String::new();
    first.push_str(stdin.unwrap_or_default());
    first.push_str(file_text);
    let mut rest = messages.iter();
    if let Some(message) = rest.next() {
        first.push_str(message);
    }
    let mut prompts = Vec::new();
    if stdin.is_some() || !file_text.is_empty() || !messages.is_empty() {
        prompts.push(first);
    }
    prompts.extend(rest.cloned());
    prompts
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{build_prompts, read_file_arguments};

    fn temp(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nyan-initial-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn prompts_concatenate_stdin_files_and_the_first_message_without_separators() {
        let messages = strings(&["first", "second"]);
        assert_eq!(
            build_prompts(Some("piped"), "<file name=\"a\">\nx\n</file>\n", &messages),
            ["piped<file name=\"a\">\nx\n</file>\nfirst", "second"]
        );
        assert_eq!(build_prompts(None, "", &messages), ["first", "second"]);
        assert_eq!(build_prompts(Some("only stdin"), "", &[]), ["only stdin"]);
        assert_eq!(build_prompts(None, "files\n", &[]), ["files\n"]);
        assert_eq!(build_prompts(None, "", &[]), Vec::<String>::new());
    }

    #[test]
    fn file_arguments_become_file_blocks_resolved_against_the_working_directory() {
        let root = temp("files");
        std::fs::write(root.join("a.txt"), "\u{feff}alpha").expect("write");
        std::fs::write(root.join("empty.txt"), "").expect("write");
        std::fs::write(root.join("b.txt"), "beta\n").expect("write");
        let text =
            read_file_arguments(&strings(&["a.txt", "empty.txt", "b.txt"]), &root).expect("read");
        assert_eq!(
            text,
            format!(
                "<file name=\"{a}\">\nalpha\n</file>\n<file name=\"{b}\">\nbeta\n\n</file>\n",
                a = root.join("a.txt").display(),
                b = root.join("b.txt").display()
            )
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_file_and_an_image_are_errors_not_silent_omissions() {
        let root = temp("errors");
        let missing = read_file_arguments(&strings(&["nope.txt"]), &root).expect_err("missing");
        assert_eq!(
            missing,
            format!("File not found: {}", root.join("nope.txt").display())
        );
        std::fs::write(root.join("pic.png"), b"\x89PNG\r\n\x1a\nrest").expect("write");
        let image = read_file_arguments(&strings(&["pic.png"]), &root).expect_err("image");
        assert!(
            image.ends_with("is an image; the Rust preview cannot attach images yet"),
            "{image}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
