//! Path resolution for tool arguments, ported from `utils/paths.ts` and
//! `core/tools/path-utils.ts`.
//!
//! The macOS-only filename variants (narrow no-break space before AM/PM, NFD,
//! curly quotes) are not ported: the Rust Coding Agent targets Linux and Windows.

use std::path::{Path, PathBuf};

fn is_unicode_space(character: char) -> bool {
    matches!(
        character,
        '\u{a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// The home directory the environment advertises: `HOME` on POSIX and
/// `USERPROFILE` on Windows.
fn home_directory() -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Resolves a model-supplied path against `cwd`.
///
/// A leading `@` is dropped, Unicode space variants become ordinary spaces, and
/// a leading `~` expands to the home directory. Relative results are joined to `cwd`.
#[must_use]
pub fn resolve_to_cwd(path: &str, cwd: &Path) -> PathBuf {
    let normalized: String = path
        .chars()
        .map(|character| {
            if is_unicode_space(character) {
                ' '
            } else {
                character
            }
        })
        .collect();
    let stripped = normalized.strip_prefix('@').unwrap_or(&normalized);
    let expanded = if stripped == "~" {
        home_directory()
    } else if let Some(rest) = stripped
        .strip_prefix("~/")
        .or_else(|| stripped.strip_prefix("~\\"))
    {
        home_directory().map(|home| home.join(rest))
    } else {
        None
    };
    let candidate = expanded.unwrap_or_else(|| PathBuf::from(stripped));
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::resolve_to_cwd;

    fn cwd() -> PathBuf {
        std::env::temp_dir().join("project")
    }

    #[test]
    fn relative_paths_join_the_working_directory() {
        assert_eq!(resolve_to_cwd("src/a.rs", &cwd()), cwd().join("src/a.rs"));
        assert_eq!(resolve_to_cwd(".", &cwd()), cwd().join("."));
    }

    #[test]
    fn absolute_paths_are_kept() {
        let absolute = std::env::temp_dir().join("elsewhere");
        assert_eq!(
            resolve_to_cwd(absolute.to_str().expect("utf8 path"), &cwd()),
            absolute
        );
    }

    #[test]
    fn a_leading_at_sign_is_dropped_and_unicode_spaces_become_spaces() {
        assert_eq!(
            resolve_to_cwd("@notes\u{202f}final.md", &cwd()),
            cwd().join("notes final.md")
        );
    }

    #[test]
    fn tilde_expands_only_at_the_start_of_a_path() {
        let resolved = resolve_to_cwd("a/~/b", &cwd());
        assert_eq!(resolved, cwd().join("a/~/b"));
        let home = resolve_to_cwd("~/x", Path::new("/unused"));
        assert!(home.is_absolute() || std::env::var_os("HOME").is_none());
        assert!(home.ends_with("x"));
    }
}
