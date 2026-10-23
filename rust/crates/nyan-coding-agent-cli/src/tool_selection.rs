//! Chooses which built-in tools are active, from `--tools`, `--exclude-tools`,
//! `--no-tools`, and `--no-builtin-tools`.
//!
//! Semantics follow `core/settings-manager.ts` and the help text: the default
//! set is `read`, `bash`, `edit`, `write`; `--tools` is an allowlist whose entries
//! may contain `*`, or a list made only of `+name` and `-name` entries that
//! adjust the default set in order; `--exclude-tools` removes matches afterwards.
//! There are no extension or MCP tools in the Rust preview, so `--no-tools` and
//! `--no-builtin-tools` both leave the model with no tools.

use crate::args::Args;

/// The tools active when no flag says otherwise, in the TypeScript order.
pub const DEFAULT_TOOLS: [&str; 4] = ["read", "bash", "edit", "write"];

/// Every tool the Rust preview can offer.
pub const ALL_TOOLS: [&str; 7] = ["read", "bash", "edit", "write", "grep", "find", "ls"];

/// Matches `name` against a pattern in which `*` stands for any run of characters.
#[must_use]
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            let Some(after) = rest.strip_prefix(part) else {
                return false;
            };
            rest = after;
        } else if index == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(found) = rest.find(part) else {
                return false;
            };
            rest = &rest[found + part.len()..];
        }
    }
    true
}

fn is_modifier(entry: &str) -> bool {
    entry.starts_with('+') || entry.starts_with('-')
}

/// Resolves the active tool names, or explains why the selection is invalid.
///
/// # Errors
///
/// Returns a message naming the entry that matches no built-in tool.
pub fn select_tools(args: &Args) -> Result<Vec<&'static str>, String> {
    if args.no_tools || args.no_builtin_tools {
        return Ok(Vec::new());
    }
    let mut active: Vec<&'static str> = DEFAULT_TOOLS.to_vec();
    if let Some(entries) = &args.tools {
        if entries.iter().all(|entry| is_modifier(entry)) && !entries.is_empty() {
            for entry in entries {
                let name = &entry[1..];
                let known = ALL_TOOLS
                    .iter()
                    .find(|tool| **tool == name)
                    .copied()
                    .ok_or_else(|| unknown_tool(name))?;
                if entry.starts_with('+') {
                    if !active.contains(&known) {
                        active.push(known);
                    }
                } else {
                    active.retain(|tool| *tool != known);
                }
            }
        } else {
            let mut chosen = Vec::new();
            for entry in entries {
                let matched: Vec<&'static str> = ALL_TOOLS
                    .iter()
                    .copied()
                    .filter(|tool| wildcard_match(entry, tool))
                    .collect();
                if matched.is_empty() {
                    return Err(unknown_tool(entry));
                }
                for tool in matched {
                    if !chosen.contains(&tool) {
                        chosen.push(tool);
                    }
                }
            }
            active = ALL_TOOLS
                .iter()
                .copied()
                .filter(|tool| chosen.contains(tool))
                .collect();
        }
    }
    if let Some(excluded) = &args.exclude_tools {
        active.retain(|tool| !excluded.iter().any(|pattern| wildcard_match(pattern, tool)));
    }
    Ok(active)
}

fn unknown_tool(name: &str) -> String {
    format!(
        "unknown tool \"{name}\": the Rust preview offers {}",
        ALL_TOOLS.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::{select_tools, wildcard_match};
    use crate::args::parse_args;

    fn select(args: &[&str]) -> Result<Vec<&'static str>, String> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        select_tools(&parse_args(&args))
    }

    #[test]
    fn wildcards_match_runs_of_characters() {
        assert!(wildcard_match("read", "read"));
        assert!(!wildcard_match("read", "reads"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("gr*", "grep"));
        assert!(wildcard_match("*d", "find"));
        assert!(wildcard_match("f*d", "find"));
        assert!(!wildcard_match("f*x", "find"));
        assert!(wildcard_match("r*a*d", "read"));
        assert!(!wildcard_match("*ep", "read"));
    }

    #[test]
    fn the_default_set_is_the_typescript_default() {
        assert_eq!(
            select(&[]).expect("select"),
            ["read", "bash", "edit", "write"]
        );
    }

    #[test]
    fn an_allowlist_replaces_the_defaults_and_keeps_declaration_order() {
        assert_eq!(
            select(&["--tools", "ls,grep"]).expect("select"),
            ["grep", "ls"]
        );
        assert_eq!(select(&["--tools", "*"]).expect("select").len(), 7);
        assert_eq!(
            select(&["-t", "g*,read"]).expect("select"),
            ["read", "grep"]
        );
    }

    #[test]
    fn modifiers_adjust_the_defaults_in_order() {
        assert_eq!(
            select(&["--tools", "+grep,-bash"]).expect("select"),
            ["read", "edit", "write", "grep"]
        );
        assert_eq!(
            select(&["--tools", "-read,+read"]).expect("select"),
            ["bash", "edit", "write", "read"]
        );
    }

    #[test]
    fn exclusion_applies_after_selection() {
        assert_eq!(
            select(&["--exclude-tools", "bash,w*"]).expect("select"),
            ["read", "edit"]
        );
        assert_eq!(
            select(&["--tools", "*", "--exclude-tools", "grep,find,ls"]).expect("select"),
            ["read", "bash", "edit", "write"]
        );
    }

    #[test]
    fn no_tools_flags_leave_nothing() {
        assert_eq!(select(&["--no-tools"]).expect("select"), Vec::<&str>::new());
        assert_eq!(select(&["-nbt"]).expect("select"), Vec::<&str>::new());
    }

    #[test]
    fn an_unknown_name_is_an_error_that_lists_what_exists() {
        assert_eq!(
            select(&["--tools", "nope"]),
            Err("unknown tool \"nope\": the Rust preview offers read, bash, edit, write, grep, find, ls".to_owned())
        );
        assert!(select(&["--tools", "+nope"]).is_err());
    }
}
