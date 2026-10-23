//! Command-line argument parsing.
//!
//! This is a branch-for-branch port of `packages/coding-agent/src/cli/args.ts`.
//! The order of the branches is part of the contract: a flag that needs a value
//! but is the last argument is not an error here, it falls through to the
//! unknown-flag rules exactly as the TypeScript parser does.

/// Output mode selected by `--mode`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Plain text output.
    Text,
    /// Strict JSONL events.
    Json,
    /// Bidirectional JSONL protocol.
    Rpc,
}

/// Interactive layout selected by `--tui-mode`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiMode {
    /// Alternate-screen layout.
    Fullscreen,
    /// Scrollback-preserving layout.
    Regular,
}

/// Value of a flag the parser does not own (possibly an extension flag).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlagValue {
    /// The flag was present without a value.
    Bool(bool),
    /// The flag carried a value.
    Text(String),
}

/// Severity of a parse diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {
    /// The run continues.
    Warning,
    /// The run must stop.
    Error,
}

/// One parse diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Severity.
    pub kind: DiagnosticKind,
    /// Message shown to the user.
    pub message: String,
}

/// `--list-models` with or without a search pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListModels {
    /// List every model.
    All,
    /// List models matching the pattern.
    Search(String),
}

/// Parsed command line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// `--provider`.
    pub provider: Option<String>,
    /// `--model`.
    pub model: Option<String>,
    /// `--api-key`.
    pub api_key: Option<String>,
    /// `--system-prompt`.
    pub system_prompt: Option<String>,
    /// `--append-system-prompt`, repeatable.
    pub append_system_prompt: Option<Vec<String>>,
    /// `--thinking`.
    pub thinking: Option<String>,
    /// `--continue`.
    pub continue_session: bool,
    /// `--resume`.
    pub resume: bool,
    /// `--help`.
    pub help: bool,
    /// `--version`.
    pub version: bool,
    /// `--mode`.
    pub mode: Option<Mode>,
    /// `--name`.
    pub name: Option<String>,
    /// `--no-session`.
    pub no_session: bool,
    /// `--session`.
    pub session: Option<String>,
    /// `--session-id`.
    pub session_id: Option<String>,
    /// `--fork`.
    pub fork: Option<String>,
    /// `--session-dir`.
    pub session_dir: Option<String>,
    /// `--models`.
    pub models: Option<Vec<String>>,
    /// `--tools`.
    pub tools: Option<Vec<String>>,
    /// `--exclude-tools`.
    pub exclude_tools: Option<Vec<String>>,
    /// `--no-tools`.
    pub no_tools: bool,
    /// `--no-builtin-tools`.
    pub no_builtin_tools: bool,
    /// `--extension`, repeatable.
    pub extensions: Option<Vec<String>>,
    /// `--no-extensions`.
    pub no_extensions: bool,
    /// `--no-mcp`.
    pub no_mcp: bool,
    /// `--print`.
    pub print: bool,
    /// `--export`.
    pub export: Option<String>,
    /// `--no-skills`.
    pub no_skills: bool,
    /// `--skill`, repeatable.
    pub skills: Option<Vec<String>>,
    /// `--prompt-template`, repeatable.
    pub prompt_templates: Option<Vec<String>>,
    /// `--no-prompt-templates`.
    pub no_prompt_templates: bool,
    /// `--theme`, repeatable.
    pub themes: Option<Vec<String>>,
    /// `--use-theme`.
    pub use_theme: Option<String>,
    /// `--no-themes`.
    pub no_themes: bool,
    /// `--no-context-files`.
    pub no_context_files: bool,
    /// `--list-models`.
    pub list_models: Option<ListModels>,
    /// `--offline`.
    pub offline: bool,
    /// `--tui-mode`.
    pub tui_mode: Option<TuiMode>,
    /// `--verbose`.
    pub verbose: bool,
    /// `--approve` (`Some(true)`) or `--no-approve` (`Some(false)`).
    pub project_trust_override: Option<bool>,
    /// Positional messages.
    pub messages: Vec<String>,
    /// `@file` arguments without the `@`.
    pub file_args: Vec<String>,
    /// Unknown long flags in first-seen order; a repeated flag keeps its slot.
    pub unknown_flags: Vec<(String, FlagValue)>,
    /// Parse diagnostics in the order they were produced.
    pub diagnostics: Vec<Diagnostic>,
}

const VALID_THINKING_LEVELS: [&str; 7] =
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Trims a session name; a blank name is no name.
#[must_use]
pub fn normalize_session_name(value: &str) -> Option<String> {
    let name = value.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_tool_modifier(entry: &str) -> bool {
    entry.starts_with('+') || entry.starts_with('-')
}

fn tool_list_error(entries: &[String]) -> Option<String> {
    let modifiers = entries
        .iter()
        .filter(|entry| is_tool_modifier(entry))
        .count();
    if modifiers == 0 {
        return None;
    }
    if modifiers < entries.len() {
        return Some("tool names cannot be mixed with +name or -name entries".to_owned());
    }
    entries
        .iter()
        .find(|entry| entry.contains('*'))
        .map(|pattern| {
            format!("+name and -name entries take exact tool names, not patterns: {pattern}")
        })
}

fn set_unknown(flags: &mut Vec<(String, FlagValue)>, name: &str, value: FlagValue) {
    if let Some(slot) = flags.iter_mut().find(|(existing, _)| existing == name) {
        slot.1 = value;
    } else {
        flags.push((name.to_owned(), value));
    }
}

/// Parses `args` (without the program name).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_args(args: &[String]) -> Args {
    let mut result = Args::default();
    let error = |result: &mut Args, message: String| {
        result.diagnostics.push(Diagnostic {
            kind: DiagnosticKind::Error,
            message,
        });
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let has_next = i + 1 < args.len();
        match () {
            () if arg == "--" => {
                for positional in &args[i + 1..] {
                    if let Some(file) = positional.strip_prefix('@') {
                        result.file_args.push(file.to_owned());
                    } else {
                        result.messages.push(positional.clone());
                    }
                }
                break;
            }
            () if arg == "--help" || arg == "-h" => result.help = true,
            () if arg == "--version" || arg == "-v" => result.version = true,
            () if arg == "--mode" => match args.get(i + 1).map(String::as_str) {
                None => error(&mut result, "--mode requires text, json, or rpc".to_owned()),
                Some(mode) if mode.starts_with('-') => {
                    error(&mut result, "--mode requires text, json, or rpc".to_owned());
                }
                Some(mode) => {
                    i += 1;
                    match mode {
                        "text" => result.mode = Some(Mode::Text),
                        "json" => result.mode = Some(Mode::Json),
                        "rpc" => result.mode = Some(Mode::Rpc),
                        other => error(
                            &mut result,
                            format!("Invalid mode \"{other}\". Valid values: text, json, rpc"),
                        ),
                    }
                }
            },
            () if arg == "--continue" || arg == "-c" => result.continue_session = true,
            () if arg == "--resume" || arg == "-r" => result.resume = true,
            () if arg == "--provider" && has_next => {
                i += 1;
                result.provider = Some(args[i].clone());
            }
            () if arg == "--model" && has_next => {
                i += 1;
                result.model = Some(args[i].clone());
            }
            () if arg == "--api-key" && has_next => {
                i += 1;
                result.api_key = Some(args[i].clone());
            }
            () if arg == "--system-prompt" && has_next => {
                i += 1;
                result.system_prompt = Some(args[i].clone());
            }
            () if arg == "--append-system-prompt" && has_next => {
                i += 1;
                result
                    .append_system_prompt
                    .get_or_insert_with(Vec::new)
                    .push(args[i].clone());
            }
            () if arg == "--name" || arg == "-n" => {
                if has_next {
                    i += 1;
                    result.name = Some(args[i].clone());
                } else {
                    error(&mut result, "--name requires a value".to_owned());
                }
            }
            () if arg == "--no-session" => result.no_session = true,
            () if arg == "--session" && has_next => {
                i += 1;
                result.session = Some(args[i].clone());
            }
            () if arg == "--session-id" && has_next => {
                i += 1;
                result.session_id = Some(args[i].clone());
            }
            () if arg == "--fork" && has_next => {
                i += 1;
                result.fork = Some(args[i].clone());
            }
            () if arg == "--session-dir" && has_next => {
                i += 1;
                result.session_dir = Some(args[i].clone());
            }
            () if arg == "--models" && has_next => {
                i += 1;
                result.models = Some(split_list(&args[i]));
            }
            () if arg == "--no-tools" || arg == "-nt" => result.no_tools = true,
            () if arg == "--no-builtin-tools" || arg == "-nbt" => result.no_builtin_tools = true,
            () if (arg == "--tools" || arg == "-t") && has_next => {
                i += 1;
                let tools = split_list(&args[i]);
                if let Some(message) = tool_list_error(&tools) {
                    error(&mut result, format!("{arg}: {message}"));
                } else {
                    result.tools = Some(tools);
                }
            }
            () if (arg == "--exclude-tools" || arg == "-xt") && has_next => {
                i += 1;
                result.exclude_tools = Some(split_list(&args[i]));
            }
            () if arg == "--thinking" && has_next => {
                i += 1;
                let level = args[i].as_str();
                if VALID_THINKING_LEVELS.contains(&level) {
                    result.thinking = Some(level.to_owned());
                } else {
                    result.diagnostics.push(Diagnostic {
                        kind: DiagnosticKind::Warning,
                        message: format!(
                            "Invalid thinking level \"{level}\". Valid values: {}",
                            VALID_THINKING_LEVELS.join(", ")
                        ),
                    });
                }
            }
            () if arg == "--print" || arg == "-p" => {
                result.print = true;
                if let Some(next) = args.get(i + 1)
                    && !next.starts_with('@')
                    && (!next.starts_with('-') || next.starts_with("---"))
                {
                    result.messages.push(next.clone());
                    i += 1;
                }
            }
            () if arg == "--export" && has_next => {
                i += 1;
                result.export = Some(args[i].clone());
            }
            () if (arg == "--extension" || arg == "-e") && has_next => {
                i += 1;
                result
                    .extensions
                    .get_or_insert_with(Vec::new)
                    .push(args[i].clone());
            }
            () if arg == "--no-extensions" || arg == "-ne" => result.no_extensions = true,
            () if arg == "--no-mcp" => result.no_mcp = true,
            () if arg == "--skill" && has_next => {
                i += 1;
                result
                    .skills
                    .get_or_insert_with(Vec::new)
                    .push(args[i].clone());
            }
            () if arg == "--prompt-template" && has_next => {
                i += 1;
                result
                    .prompt_templates
                    .get_or_insert_with(Vec::new)
                    .push(args[i].clone());
            }
            () if arg == "--theme" && has_next => {
                i += 1;
                result
                    .themes
                    .get_or_insert_with(Vec::new)
                    .push(args[i].clone());
            }
            () if arg == "--use-theme" => match args.get(i + 1) {
                Some(name) if !name.starts_with('-') => {
                    result.use_theme = Some(name.clone());
                    i += 1;
                }
                _ => error(&mut result, "--use-theme requires a theme name".to_owned()),
            },
            () if arg == "--no-skills" || arg == "-ns" => result.no_skills = true,
            () if arg == "--no-prompt-templates" || arg == "-np" => {
                result.no_prompt_templates = true;
            }
            () if arg == "--no-themes" => result.no_themes = true,
            () if arg == "--no-context-files" || arg == "-nc" => result.no_context_files = true,
            () if arg == "--list-models" => match args.get(i + 1) {
                Some(next) if !next.starts_with('-') && !next.starts_with('@') => {
                    result.list_models = Some(ListModels::Search(next.clone()));
                    i += 1;
                }
                _ => result.list_models = Some(ListModels::All),
            },
            () if arg == "--tui-mode" => match args.get(i + 1).map(String::as_str) {
                Some("regular") => {
                    result.tui_mode = Some(TuiMode::Regular);
                    i += 1;
                }
                Some("fullscreen") => {
                    result.tui_mode = Some(TuiMode::Fullscreen);
                    i += 1;
                }
                None => error(
                    &mut result,
                    "--tui-mode requires regular or fullscreen".to_owned(),
                ),
                Some(mode) if mode.starts_with('-') => error(
                    &mut result,
                    "--tui-mode requires regular or fullscreen".to_owned(),
                ),
                Some(mode) => {
                    i += 1;
                    error(
                        &mut result,
                        format!("Invalid TUI mode \"{mode}\". Valid values: regular, fullscreen"),
                    );
                }
            },
            () if arg == "--verbose" => result.verbose = true,
            () if arg == "--approve" || arg == "-a" => result.project_trust_override = Some(true),
            () if arg == "--no-approve" || arg == "-na" => {
                result.project_trust_override = Some(false);
            }
            () if arg == "--offline" => result.offline = true,
            () if arg.starts_with('@') => result.file_args.push(arg[1..].to_owned()),
            () if arg.starts_with("--") => {
                if let Some(equals) = arg.find('=') {
                    set_unknown(
                        &mut result.unknown_flags,
                        &arg[2..equals],
                        FlagValue::Text(arg[equals + 1..].to_owned()),
                    );
                } else {
                    let name = &arg[2..];
                    match args.get(i + 1) {
                        Some(next) if !next.starts_with('-') && !next.starts_with('@') => {
                            set_unknown(
                                &mut result.unknown_flags,
                                name,
                                FlagValue::Text(next.clone()),
                            );
                            i += 1;
                        }
                        _ => set_unknown(&mut result.unknown_flags, name, FlagValue::Bool(true)),
                    }
                }
            }
            () if arg.starts_with('-') => error(&mut result, format!("Unknown option: {arg}")),
            () => result.messages.push(arg.to_owned()),
        }
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticKind, FlagValue, ListModels, Mode, TuiMode, normalize_session_name, parse_args,
    };

    fn parse(args: &[&str]) -> super::Args {
        parse_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    fn errors(args: &super::Args) -> Vec<&str> {
        args.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == DiagnosticKind::Error)
            .map(|diagnostic| diagnostic.message.as_str())
            .collect()
    }

    #[test]
    fn positionals_files_and_double_dash() {
        let args = parse(&["hello", "@a.md", "--", "- dash", "@b.md", "--mode"]);
        assert_eq!(args.messages, ["hello", "- dash", "--mode"]);
        assert_eq!(args.file_args, ["a.md", "b.md"]);
        assert_eq!(args.diagnostics.len(), 0);
    }

    #[test]
    fn mode_values_and_errors() {
        assert_eq!(parse(&["--mode", "json"]).mode, Some(Mode::Json));
        assert_eq!(parse(&["--mode", "rpc"]).mode, Some(Mode::Rpc));
        assert_eq!(parse(&["--mode", "text"]).mode, Some(Mode::Text));
        let missing = parse(&["--mode"]);
        assert_eq!(errors(&missing), ["--mode requires text, json, or rpc"]);
        let dash = parse(&["--mode", "--print"]);
        assert_eq!(errors(&dash), ["--mode requires text, json, or rpc"]);
        assert!(dash.print);
        let invalid = parse(&["--mode", "xml", "after"]);
        assert_eq!(
            errors(&invalid),
            ["Invalid mode \"xml\". Valid values: text, json, rpc"]
        );
        assert_eq!(invalid.messages, ["after"]);
    }

    #[test]
    fn valued_flag_without_a_value_falls_through_to_unknown_flag() {
        let args = parse(&["--model"]);
        assert_eq!(args.model, None);
        assert_eq!(
            args.unknown_flags,
            [("model".to_owned(), FlagValue::Bool(true))]
        );
    }

    #[test]
    fn print_consumes_a_prompt_unless_it_looks_like_an_option_or_file() {
        assert_eq!(parse(&["-p", "list files"]).messages, ["list files"]);
        let option = parse(&["-p", "--verbose"]);
        assert!(option.print && option.verbose);
        assert_eq!(option.messages, Vec::<String>::new());
        let file = parse(&["-p", "@x.md"]);
        assert_eq!(file.file_args, ["x.md"]);
        assert_eq!(file.messages.len(), 0);
        assert_eq!(parse(&["-p", "---raw"]).messages, ["---raw"]);
    }

    #[test]
    fn tool_lists_trim_and_validate_modifiers() {
        assert_eq!(
            parse(&["--tools", " read , bash ,,"]).tools,
            Some(vec!["read".to_owned(), "bash".to_owned()])
        );
        let mixed = parse(&["-t", "read,+bash"]);
        assert_eq!(
            errors(&mixed),
            ["-t: tool names cannot be mixed with +name or -name entries"]
        );
        assert_eq!(mixed.tools, None);
        let pattern = parse(&["--tools", "+bash*"]);
        assert_eq!(
            errors(&pattern),
            ["--tools: +name and -name entries take exact tool names, not patterns: +bash*"]
        );
        assert_eq!(
            parse(&["--tools", "+a,-b"]).tools,
            Some(vec!["+a".to_owned(), "-b".to_owned()])
        );
    }

    #[test]
    fn thinking_level_is_a_warning_not_an_error() {
        assert_eq!(
            parse(&["--thinking", "high"]).thinking.as_deref(),
            Some("high")
        );
        let args = parse(&["--thinking", "ultra"]);
        assert_eq!(args.thinking, None);
        assert_eq!(args.diagnostics.len(), 1);
        assert_eq!(args.diagnostics[0].kind, DiagnosticKind::Warning);
        assert_eq!(
            args.diagnostics[0].message,
            "Invalid thinking level \"ultra\". Valid values: off, minimal, low, medium, high, xhigh, max"
        );
    }

    #[test]
    fn repeatable_and_list_flags() {
        let args = parse(&[
            "-e",
            "a",
            "--extension",
            "b",
            "--skill",
            "s",
            "--theme",
            "t",
            "--prompt-template",
            "p",
            "--append-system-prompt",
            "x",
            "--append-system-prompt",
            "y",
            "--models",
            "a, b,,c",
        ]);
        assert_eq!(args.extensions, Some(vec!["a".to_owned(), "b".to_owned()]));
        assert_eq!(args.skills, Some(vec!["s".to_owned()]));
        assert_eq!(args.themes, Some(vec!["t".to_owned()]));
        assert_eq!(args.prompt_templates, Some(vec!["p".to_owned()]));
        assert_eq!(
            args.append_system_prompt,
            Some(vec!["x".to_owned(), "y".to_owned()])
        );
        assert_eq!(
            args.models,
            Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
        );
    }

    #[test]
    fn list_models_takes_an_optional_search() {
        assert_eq!(
            parse(&["--list-models", "sonnet"]).list_models,
            Some(ListModels::Search("sonnet".to_owned()))
        );
        let bare = parse(&["--list-models", "--verbose"]);
        assert_eq!(bare.list_models, Some(ListModels::All));
        assert!(bare.verbose);
        assert_eq!(
            parse(&["--list-models", "@f"]).list_models,
            Some(ListModels::All)
        );
    }

    #[test]
    fn tui_mode_and_use_theme_diagnostics() {
        assert_eq!(
            parse(&["--tui-mode", "regular"]).tui_mode,
            Some(TuiMode::Regular)
        );
        assert_eq!(
            parse(&["--tui-mode", "fullscreen"]).tui_mode,
            Some(TuiMode::Fullscreen)
        );
        assert_eq!(
            errors(&parse(&["--tui-mode", "wide"])),
            ["Invalid TUI mode \"wide\". Valid values: regular, fullscreen"]
        );
        assert_eq!(
            errors(&parse(&["--tui-mode"])),
            ["--tui-mode requires regular or fullscreen"]
        );
        assert_eq!(
            errors(&parse(&["--use-theme", "--verbose"])),
            ["--use-theme requires a theme name"]
        );
        assert_eq!(
            parse(&["--use-theme", "dark/light"]).use_theme.as_deref(),
            Some("dark/light")
        );
    }

    #[test]
    fn trust_session_and_boolean_flags() {
        assert_eq!(parse(&["-a"]).project_trust_override, Some(true));
        assert_eq!(parse(&["--no-approve"]).project_trust_override, Some(false));
        assert_eq!(parse(&["-na"]).project_trust_override, Some(false));
        let args = parse(&[
            "-c",
            "-r",
            "--no-session",
            "--offline",
            "-nt",
            "-nbt",
            "-ne",
            "--no-mcp",
            "-ns",
            "-np",
            "--no-themes",
            "-nc",
            "--verbose",
            "-h",
            "-v",
        ]);
        assert!(args.continue_session && args.resume && args.no_session && args.offline);
        assert!(args.no_tools && args.no_builtin_tools && args.no_extensions && args.no_mcp);
        assert!(args.no_skills && args.no_prompt_templates && args.no_themes);
        assert!(args.no_context_files && args.verbose && args.help && args.version);
        assert_eq!(errors(&parse(&["--name"])), ["--name requires a value"]);
        assert_eq!(parse(&["-n", "My run"]).name.as_deref(), Some("My run"));
    }

    #[test]
    fn unknown_long_flags_are_forwarded_and_short_flags_are_rejected() {
        let args = parse(&["--plan", "--level=3", "--out", "file", "--flag", "@x", "-z"]);
        assert_eq!(
            args.unknown_flags,
            [
                ("plan".to_owned(), FlagValue::Bool(true)),
                ("level".to_owned(), FlagValue::Text("3".to_owned())),
                ("out".to_owned(), FlagValue::Text("file".to_owned())),
                ("flag".to_owned(), FlagValue::Bool(true)),
            ]
        );
        assert_eq!(args.file_args, ["x"]);
        assert_eq!(errors(&args), ["Unknown option: -z"]);
    }

    #[test]
    fn a_repeated_unknown_flag_keeps_its_first_position_and_last_value() {
        let args = parse(&["--a=1", "--b=2", "--a=3"]);
        assert_eq!(
            args.unknown_flags,
            [
                ("a".to_owned(), FlagValue::Text("3".to_owned())),
                ("b".to_owned(), FlagValue::Text("2".to_owned())),
            ]
        );
    }

    #[test]
    fn session_name_is_trimmed_and_blank_is_none() {
        assert_eq!(normalize_session_name("  hi  ").as_deref(), Some("hi"));
        assert_eq!(normalize_session_name("   "), None);
    }
}
