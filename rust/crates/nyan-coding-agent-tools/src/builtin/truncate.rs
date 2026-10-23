//! Output truncation shared by the built-in tools.
//!
//! Two independent limits apply and whichever is hit first wins: a line limit
//! and a byte limit. Truncation never returns a partial line, except for the
//! tail case where the last line alone exceeds the byte limit.
//! Ported from `core/tools/truncate.ts`.

/// Default maximum number of lines.
pub const DEFAULT_MAX_LINES: usize = 2000;
/// Default maximum number of bytes.
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// Which limit truncated the content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TruncatedBy {
    /// The line limit was hit first.
    Lines,
    /// The byte limit was hit first.
    Bytes,
}

/// The outcome of a truncation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Truncation {
    /// The kept content.
    pub content: String,
    /// Whether anything was dropped.
    pub truncated: bool,
    /// The limit that was hit, when truncated.
    pub truncated_by: Option<TruncatedBy>,
    /// Lines in the original content.
    pub total_lines: usize,
    /// Bytes in the original content.
    pub total_bytes: usize,
    /// Complete lines in the kept content.
    pub output_lines: usize,
    /// Bytes in the kept content.
    pub output_bytes: usize,
    /// Whether the first line alone exceeds the byte limit (head truncation).
    pub first_line_exceeds_limit: bool,
}

/// Formats a byte count the way the TypeScript tools do.
#[must_use]
pub fn format_size(bytes: usize) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", value / 1024.0)
    } else {
        format!("{:.1}MB", value / (1024.0 * 1024.0))
    }
}

fn lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Keeps the first lines and bytes of `content`.
#[must_use]
pub fn truncate_head(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let total_bytes = content.len();
    let lines = lines_for_counting(content);
    let total_lines = lines.len();
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return Truncation {
            content: content.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            first_line_exceeds_limit: false,
        };
    }
    if lines.first().is_some_and(|first| first.len() > max_bytes) {
        return Truncation {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            first_line_exceeds_limit: true,
        };
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut kept_bytes = 0;
    let mut truncated_by = TruncatedBy::Lines;
    for (index, line) in lines.iter().enumerate().take(max_lines) {
        let line_bytes = line.len() + usize::from(index > 0);
        if kept_bytes + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        kept.push(line);
        kept_bytes += line_bytes;
    }
    if kept.len() >= max_lines && kept_bytes <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }
    let content = kept.join("\n");
    Truncation {
        output_bytes: content.len(),
        output_lines: kept.len(),
        content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        first_line_exceeds_limit: false,
    }
}

#[cfg(test)]
mod tests {
    use super::{TruncatedBy, format_size, truncate_head};

    #[test]
    fn content_within_both_limits_is_untouched() {
        let result = truncate_head("a\nb\nc\n", 10, 100);
        assert!(!result.truncated);
        assert_eq!(result.content, "a\nb\nc\n");
        assert_eq!((result.total_lines, result.output_lines), (3, 3));
    }

    #[test]
    fn the_line_limit_keeps_whole_lines() {
        let result = truncate_head("1\n2\n3\n4\n5", 3, 1000);
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.content, "1\n2\n3");
        assert_eq!((result.total_lines, result.output_lines), (5, 3));
    }

    #[test]
    fn the_byte_limit_never_splits_a_line() {
        let result = truncate_head("aaaa\nbbbb\ncccc", 100, 9);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(result.content, "aaaa\nbbbb");
        assert_eq!(result.output_bytes, 9);
    }

    #[test]
    fn a_first_line_over_the_byte_limit_yields_nothing_and_says_so() {
        let result = truncate_head("xxxxxxxxxx\nshort", 100, 5);
        assert!(result.first_line_exceeds_limit);
        assert_eq!(result.content, "");
        assert_eq!(result.output_lines, 0);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn empty_content_has_no_lines() {
        let result = truncate_head("", 10, 10);
        assert_eq!((result.total_lines, result.truncated), (0, false));
    }

    #[test]
    fn sizes_format_like_the_typescript_tools() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1023), "1023B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(50 * 1024), "50.0KB");
        assert_eq!(format_size(3 * 1024 * 1024 / 2), "1.5MB");
    }
}
