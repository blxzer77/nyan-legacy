//! The process entry point as a library function.
//!
//! Keeping the whole path (arguments, diagnostics, mode, provider, runtime,
//! output) behind one function lets tests drive the same code the executable
//! runs, with the environment and the provider endpoint injected.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nyan_coding_agent_session::NativeSession;

use nyan_coding_agent_core::VERSION;
use nyan_coding_agent_protocol::{RpcCommandName, SessionEventName};
use nyan_coding_agent_provider::ProviderModel;
use nyan_coding_agent_tools::builtin::BuiltinTools;

use crate::app_mode::{AppMode, resolve_app_mode};
use crate::args::{Args, DiagnosticKind, ListModels, parse_args};
use crate::headless::{HeadlessMode, HeadlessOptions, SharedWriter, run_headless};
use crate::initial_message::{build_prompts, read_file_arguments};
use crate::login::{run_list_models, run_login, run_logout};
use crate::providers::{ProviderSetup, build_provider, resolve_model, resolve_provider};
use crate::session_setup::open_session;
use crate::system_prompt::{
    PromptOptions, build_system_prompt, load_project_context_files, text_or_file,
};
use crate::tool_selection::select_tools;

/// Looks up an environment variable.
pub type EnvLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Asks one question and returns the answer line, or `None` at end of input.
pub type PromptFn = Box<dyn Fn(&str, bool) -> Option<String> + Send + Sync>;

/// Reads whatever was piped to stdin, trimmed.
pub type StdinReader = Box<dyn Fn() -> Option<String> + Send + Sync>;

/// Everything the process takes from its surroundings.
pub struct Environment {
    /// Looks up an environment variable.
    pub var: EnvLookup,
    /// Whether stdin is a terminal.
    pub stdin_is_tty: bool,
    /// Whether stdout is a terminal.
    pub stdout_is_tty: bool,
    /// Working directory.
    pub cwd: PathBuf,
    /// Base URLs that replace a provider's own, by provider id.
    ///
    /// This is the injection point tests use to reach a loopback server and
    /// where a `models.json` override will land; the executable passes none.
    pub base_urls: BTreeMap<String, String>,
    /// Where the model data is fetched from.
    pub models_dev_url: String,
    /// Reads piped stdin: `None` when stdin is a terminal or holds only whitespace.
    pub read_stdin: StdinReader,
    /// Asks the user one question (`secret` hides the answer); `None` when input has ended.
    pub prompt: PromptFn,
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Environment")
            .field("stdin_is_tty", &self.stdin_is_tty)
            .field("stdout_is_tty", &self.stdout_is_tty)
            .field("cwd", &self.cwd)
            .field("base_urls", &self.base_urls)
            .field("models_dev_url", &self.models_dev_url)
            .finish_non_exhaustive()
    }
}

fn help_text() -> String {
    let lines = [
        format!("nyan-coding-agent {VERSION}"),
        String::new(),
        "Rust Coding Agent preview. It is not a usable replacement yet.".to_owned(),
        String::new(),
        format!(
            "Protocol contracts frozen: {} RPC commands, {} session events.",
            RpcCommandName::all().len(),
            SessionEventName::all().len()
        ),
        String::new(),
        "Supported now: --print and --mode json; the read, bash, edit, write, grep, find,"
            .to_owned(),
        "and ls tools; sessions, @files, and piped stdin. Providers (API key from the".to_owned(),
        "environment, --api-key, or `nyan login <provider>`): CLIProxyAPI, OpenAI, xAI,".to_owned(),
        "Anthropic, Google, Google Vertex (API key), and the OpenAI-compatible".to_owned(),
        "providers. Providers that mix wire APIs per model and OAuth logins are not".to_owned(),
        "supported yet.".to_owned(),
        String::new(),
        "Commands:".to_owned(),
        "  login [provider]    Store a login and choose a default model".to_owned(),
        "  logout [provider]   Remove a stored login".to_owned(),
        String::new(),
        "Options:".to_owned(),
        "  -h, --help          Show this help".to_owned(),
        "  -v, --version       Show the version".to_owned(),
        "  --list-models [s]   List the models of logged-in providers".to_owned(),
    ];
    lines.join("\n") + "\n"
}

fn write_to(writer: &SharedWriter, text: &str) {
    if let Ok(mut writer) = writer.lock() {
        let _ = writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.flush());
    }
}

/// What the arguments ask the process to do.
#[allow(clippy::large_enum_variant)] // built once per process and moved straight out
enum Plan {
    /// Finish now with this exit code; output was already written.
    Exit(i32),
    /// Run the prompts.
    Run {
        mode: HeadlessMode,
        model: ProviderModel,
        setup: ProviderSetup,
        messages: Vec<String>,
        tools: Vec<&'static str>,
        system_prompt: String,
        session: NativeSession,
    },
}

/// The agent directory: `NYAN_CODING_AGENT_DIR`, else `~/.nyan/agent`.
pub(crate) fn agent_dir(env: &Environment) -> PathBuf {
    if let Some(dir) = (env.var)("NYAN_CODING_AGENT_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    let home_variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    (env.var)(home_variable)
        .filter(|home| !home.is_empty())
        .map_or_else(
            || PathBuf::from(".nyan").join("agent"),
            |home| PathBuf::from(home).join(".nyan").join("agent"),
        )
}

fn build_prompt(parsed: &Args, tools: &[&str], env: &Environment) -> String {
    let appended: Vec<String> = parsed
        .append_system_prompt
        .iter()
        .flatten()
        .map(|text| text_or_file(text))
        .filter(|text| !text.is_empty())
        .collect();
    build_system_prompt(&PromptOptions {
        custom_prompt: parsed
            .system_prompt
            .as_deref()
            .map(text_or_file)
            .filter(|text| !text.is_empty()),
        selected_tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        append: (!appended.is_empty()).then(|| appended.join("\n\n")),
        context_files: if parsed.no_context_files {
            Vec::new()
        } else {
            load_project_context_files(&env.cwd, &agent_dir(env))
        },
        cwd: env.cwd.clone(),
    })
}

fn fail(err: &SharedWriter, message: &str) -> Plan {
    write_to(err, &format!("Error: {message}\n"));
    Plan::Exit(1)
}

fn plan(argv: &[String], env: &Environment, out: &SharedWriter, err: &SharedWriter) -> Plan {
    let parsed = parse_args(argv);
    let mut failed = false;
    for diagnostic in &parsed.diagnostics {
        match diagnostic.kind {
            DiagnosticKind::Error => {
                failed = true;
                write_to(err, &format!("Error: {}\n", diagnostic.message));
            }
            DiagnosticKind::Warning => write_to(err, &format!("Warning: {}\n", diagnostic.message)),
        }
    }
    if failed {
        return Plan::Exit(1);
    }
    if parsed.version {
        write_to(out, &format!("{VERSION}\n"));
        return Plan::Exit(0);
    }
    if parsed.help || (argv.is_empty() && env.stdin_is_tty) {
        write_to(out, &help_text());
        return Plan::Exit(0);
    }

    if let Some(list) = &parsed.list_models {
        let search = match list {
            ListModels::Search(pattern) => Some(pattern.as_str()),
            ListModels::All => None,
        };
        return Plan::Exit(run_list_models(search, env, &agent_dir(env), out, err));
    }
    let mode = match resolve_app_mode(&parsed, env.stdin_is_tty, env.stdout_is_tty) {
        AppMode::Print => HeadlessMode::Print,
        AppMode::Json => HeadlessMode::Json,
        AppMode::Rpc | AppMode::Interactive => {
            write_to(
                err,
                "Error: this mode is not available in the Rust preview yet; use the TypeScript `nyan` command\n",
            );
            return Plan::Exit(2);
        }
    };
    if let Some((name, _)) = parsed.unknown_flags.first() {
        return fail(
            err,
            &format!(
                "unknown option --{name}: extension flags are not supported by the Rust preview yet"
            ),
        );
    }
    let agent = agent_dir(env);
    let model = match resolve_model(&parsed, &agent) {
        Ok(model) => model,
        Err(message) => return fail(err, &message),
    };
    let setup = match resolve_provider(&model.provider, &model.model, &parsed, env, &agent) {
        Ok(resolved) => {
            for note in &resolved.notes {
                write_to(err, &format!("{note}\n"));
            }
            resolved.setup
        }
        Err(message) => return fail(err, &message),
    };
    let tools = match select_tools(&parsed) {
        Ok(tools) => tools,
        Err(message) => return fail(err, &message),
    };
    let file_text = match read_file_arguments(&parsed.file_args, &env.cwd) {
        Ok(text) => text,
        Err(message) => return fail(err, &message),
    };
    let stdin = (env.read_stdin)();
    let prompts = build_prompts(stdin.as_deref(), &file_text, &parsed.messages);
    let system_prompt = build_prompt(&parsed, &tools, env);
    let session = match open_session(&parsed, &agent_dir(env), &env.cwd) {
        Ok(session) => session,
        Err(message) => return fail(err, &message),
    };
    Plan::Run {
        mode,
        model,
        setup,
        messages: prompts,
        tools,
        system_prompt,
        session,
    }
}

/// Runs the process for `argv` (without the program name) and returns its exit code.
#[must_use]
pub fn run(argv: &[String], env: &Environment, out: &SharedWriter, err: &SharedWriter) -> i32 {
    match argv.first().map(String::as_str) {
        Some("login") => return run_login(&argv[1..], env, &agent_dir(env), out, err),
        Some("logout") => return run_logout(&argv[1..], &agent_dir(env), out, err),
        _ => {}
    }
    let (mode, model, setup, messages, tools, system_prompt, session) =
        match plan(argv, env, out, err) {
            Plan::Exit(code) => return code,
            Plan::Run {
                mode,
                model,
                setup,
                messages,
                tools,
                system_prompt,
                session,
            } => (mode, model, setup, messages, tools, system_prompt, session),
        };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            write_to(
                err,
                &format!("Error: failed to start the async runtime: {error}\n"),
            );
            return 1;
        }
    };
    let providers = match build_provider(&model.provider, setup, runtime.handle().clone()) {
        Ok(providers) => providers,
        Err(error) => {
            write_to(err, &format!("Error: {error}\n"));
            return 1;
        }
    };
    runtime.block_on(run_headless(
        HeadlessOptions {
            mode,
            messages,
            cwd: env.cwd.clone(),
            model,
            system_prompt: Some(system_prompt),
        },
        &providers,
        &BuiltinTools::only(&tools),
        Arc::new(Mutex::new(session)),
        out.clone(),
        err.clone(),
    ))
}
