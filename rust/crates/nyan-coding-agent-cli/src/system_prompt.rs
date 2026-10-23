//! The system prompt and the project context it carries, ported from
//! `core/system-prompt.ts` and `loadProjectContextFiles` in `core/resource-loader.ts`.
//!
//! The prompt is built from named sections. `preamble` is plain text; every other
//! section is wrapped in a tag of its own name, and the rendered prompt joins the
//! sections with a blank line.
//!
//! Differences from the TypeScript prompt: the documentation section is left out
//! because the Rust preview ships no docs to point at; there is no skills section
//! (skills are not implemented); `bash` carries no guideline about `NYAN_*`
//! variables because the Rust `bash` tool does not add them; and the linked-worktree
//! shadowing rule for context files is not ported.

use std::path::{Path, PathBuf};

/// A project instruction file included in the prompt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextFile {
    /// Where the file was read from.
    pub path: String,
    /// Its text, without a byte-order mark.
    pub content: String,
}

/// Inputs to the prompt.
#[derive(Clone, Debug, Default)]
pub struct PromptOptions {
    /// Replaces the default preamble, tool list, rules, and docs when set.
    pub custom_prompt: Option<String>,
    /// Active tool names, in order.
    pub selected_tools: Vec<String>,
    /// Text appended after the rules, from `--append-system-prompt`.
    pub append: Option<String>,
    /// Project instruction files, outermost first.
    pub context_files: Vec<ContextFile>,
    /// Working directory.
    pub cwd: PathBuf,
}

const PREAMBLE: &str = "You are an expert coding assistant operating inside nyan, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.";

fn snippet(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "read" => "Read file contents",
        "bash" => "Execute bash commands (ls, grep, find, etc.)",
        "edit" => {
            "Make precise file edits with exact text replacement, including multiple disjoint edits in one call"
        }
        "write" => "Create or overwrite files",
        "grep" => "Search file contents for patterns (respects .gitignore)",
        "find" => "Find files by glob pattern (respects .gitignore)",
        "ls" => "List directory contents",
        _ => return None,
    })
}

fn guidelines(tool: &str) -> &'static [&'static str] {
    match tool {
        "read" => &["Use read to examine files instead of cat or sed."],
        "edit" => &[
            "Use edit for precise changes (edits[].oldText must match exactly)",
            "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
            "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
            "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
        ],
        "write" => &["Use write only for new files or complete rewrites."],
        _ => &[],
    }
}

fn rules(selected: &[String]) -> String {
    let has = |name: &str| selected.iter().any(|tool| tool == name);
    let mut rules: Vec<String> = Vec::new();
    let mut add = |rule: &str| {
        let rule = rule.trim();
        if !rule.is_empty() && !rules.iter().any(|existing| existing == rule) {
            rules.push(rule.to_owned());
        }
    };
    if has("bash") && !has("grep") && !has("find") && !has("ls") {
        add("Use bash for file operations like ls, rg, find");
    }
    for tool in selected {
        for rule in guidelines(tool) {
            add(rule);
        }
    }
    add("Be concise in your responses");
    add("Show file paths clearly when working with files");
    rules
        .iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_project_context(files: &[ContextFile]) -> String {
    let mut parts = vec!["Project-specific instructions and guidelines:".to_owned()];
    for file in files {
        parts.push(format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path, file.content
        ));
    }
    parts.join("\n\n")
}

/// Builds the system prompt text.
#[must_use]
pub fn build_system_prompt(options: &PromptOptions) -> String {
    let mut sections: Vec<(&str, String)> = Vec::new();
    let preamble = if let Some(custom) = options
        .custom_prompt
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        custom.to_owned()
    } else {
        let visible: Vec<String> = options
            .selected_tools
            .iter()
            .filter_map(|name| snippet(name).map(|text| format!("- {name}: {text}")))
            .collect();
        let tools = if visible.is_empty() {
            "(none)".to_owned()
        } else {
            visible.join("\n")
        };
        sections.push((
            "tools",
            format!(
                "{tools}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project."
            ),
        ));
        sections.push(("rules", rules(&options.selected_tools)));
        PREAMBLE.to_owned()
    };
    if let Some(append) = options.append.as_deref().filter(|text| !text.is_empty()) {
        sections.push(("addendum", append.to_owned()));
    }
    if !options.context_files.is_empty() {
        sections.push((
            "project_context",
            render_project_context(&options.context_files),
        ));
    }
    sections.push(("cwd", options.cwd.to_string_lossy().replace('\\', "/")));

    let mut parts = vec![preamble];
    for (name, content) in sections {
        parts.push(format!("<{name}>\n{content}\n</{name}>"));
    }
    parts.retain(|part| !part.is_empty());
    parts.join("\n\n")
}

const CONTEXT_FILE_NAMES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

fn strip_bom(text: String) -> String {
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    }
}

/// Reads the first context file that exists in `directory`.
fn context_file_in(directory: &Path) -> Option<ContextFile> {
    for name in CONTEXT_FILE_NAMES {
        let path = directory.join(name);
        if !path.is_file() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(ContextFile {
                path: path.to_string_lossy().into_owned(),
                content: strip_bom(String::from_utf8_lossy(&bytes).into_owned()),
            });
        }
    }
    None
}

/// Loads the global agent-directory context file, then every ancestor's, outermost first.
#[must_use]
pub fn load_project_context_files(cwd: &Path, agent_dir: &Path) -> Vec<ContextFile> {
    let mut files = Vec::new();
    if let Some(global) = context_file_in(agent_dir) {
        files.push(global);
    }
    let mut ancestors: Vec<ContextFile> = Vec::new();
    for directory in cwd.ancestors() {
        if let Some(file) = context_file_in(directory)
            && !files
                .iter()
                .chain(&ancestors)
                .any(|seen| seen.path == file.path)
        {
            ancestors.push(file);
        }
    }
    ancestors.reverse();
    files.extend(ancestors);
    files
}

/// Uses `input` as a path to a file when one exists there, otherwise as literal text.
#[must_use]
pub fn text_or_file(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    match std::fs::read(input) {
        Ok(bytes) if Path::new(input).is_file() => {
            strip_bom(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => input.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        ContextFile, PromptOptions, build_system_prompt, load_project_context_files, text_or_file,
    };

    fn options(tools: &[&str]) -> PromptOptions {
        PromptOptions {
            selected_tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            cwd: PathBuf::from("/work/project"),
            ..PromptOptions::default()
        }
    }

    #[test]
    fn the_default_prompt_lists_tools_rules_and_the_working_directory() {
        let prompt = build_system_prompt(&options(&["read", "bash", "edit", "write"]));
        let expected = concat!(
            "You are an expert coding assistant operating inside nyan, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\n",
            "<tools>\n",
            "- read: Read file contents\n",
            "- bash: Execute bash commands (ls, grep, find, etc.)\n",
            "- edit: Make precise file edits with exact text replacement, including multiple disjoint edits in one call\n",
            "- write: Create or overwrite files\n\n",
            "In addition to the tools above, you may have access to other custom tools depending on the project.\n",
            "</tools>\n\n",
            "<rules>\n",
            "- Use bash for file operations like ls, rg, find\n",
            "- Use read to examine files instead of cat or sed.\n",
            "- Use edit for precise changes (edits[].oldText must match exactly)\n",
            "- When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls\n",
            "- Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.\n",
            "- Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.\n",
            "- Use write only for new files or complete rewrites.\n",
            "- Be concise in your responses\n",
            "- Show file paths clearly when working with files\n",
            "</rules>\n\n",
            "<cwd>\n/work/project\n</cwd>"
        );
        assert_eq!(prompt, expected);
    }

    #[test]
    fn search_tools_replace_the_bash_hint_and_no_tools_says_none() {
        let with_search = build_system_prompt(&options(&["bash", "grep"]));
        assert!(!with_search.contains("Use bash for file operations like ls, rg, find"));
        let none = build_system_prompt(&options(&[]));
        assert!(none.contains("<tools>\n(none)\n\nIn addition"));
        assert!(none.contains("- Be concise in your responses"));
    }

    #[test]
    fn a_custom_prompt_replaces_the_preamble_tools_and_rules_but_keeps_context() {
        let mut custom = options(&["read"]);
        custom.custom_prompt = Some("You are a pirate.".to_owned());
        custom.append = Some("Always say arr.".to_owned());
        custom.context_files = vec![ContextFile {
            path: "/work/AGENTS.md".to_owned(),
            content: "Use tabs.".to_owned(),
        }];
        assert_eq!(
            build_system_prompt(&custom),
            concat!(
                "You are a pirate.\n\n",
                "<addendum>\nAlways say arr.\n</addendum>\n\n",
                "<project_context>\nProject-specific instructions and guidelines:\n\n",
                "<project_instructions path=\"/work/AGENTS.md\">\nUse tabs.\n</project_instructions>\n",
                "</project_context>\n\n",
                "<cwd>\n/work/project\n</cwd>"
            )
        );
    }

    #[test]
    fn windows_separators_in_the_working_directory_become_slashes() {
        let mut windows = options(&[]);
        windows.cwd = PathBuf::from("C:\\Users\\dev\\project");
        assert!(build_system_prompt(&windows).ends_with("<cwd>\nC:/Users/dev/project\n</cwd>"));
    }

    fn temp(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("nyan-prompt-{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn context_files_load_global_first_then_ancestors_outermost_first() {
        let root = temp("context");
        let agent = root.join("agent");
        let project = root.join("repo/pkg");
        std::fs::create_dir_all(&agent).expect("agent dir");
        std::fs::create_dir_all(&project).expect("project dir");
        std::fs::write(agent.join("AGENTS.md"), "global").expect("write");
        std::fs::write(root.join("repo/AGENTS.md"), "\u{feff}repo").expect("write");
        std::fs::write(project.join("CLAUDE.md"), "package").expect("write");
        std::fs::write(project.join("AGENTS.md"), "package agents wins over claude")
            .expect("write");
        let files = load_project_context_files(&project, &agent);
        let contents: Vec<&str> = files.iter().map(|file| file.content.as_str()).collect();
        assert_eq!(
            contents,
            ["global", "repo", "package agents wins over claude"]
        );
        assert!(
            files[1].path.ends_with("repo/AGENTS.md") || files[1].path.ends_with("repo\\AGENTS.md")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_override_file_beats_agents_md_in_the_same_directory() {
        let root = temp("override");
        std::fs::write(root.join("AGENTS.override.md"), "override").expect("write");
        std::fs::write(root.join("AGENTS.md"), "plain").expect("write");
        let files = load_project_context_files(&root, Path::new("/nonexistent-agent-dir"));
        assert_eq!(
            files
                .iter()
                .filter(|file| file.path.starts_with(root.to_str().expect("utf8")))
                .count(),
            1
        );
        assert_eq!(files.last().expect("a file").content, "override");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prompt_arguments_are_file_contents_when_the_path_exists_and_literal_text_otherwise() {
        let root = temp("text-or-file");
        let file = root.join("prompt.md");
        std::fs::write(&file, "from file").expect("write");
        assert_eq!(text_or_file(file.to_str().expect("utf8")), "from file");
        assert_eq!(text_or_file("just text"), "just text");
        assert_eq!(text_or_file(""), "");
        let _ = std::fs::remove_dir_all(root);
    }
}
