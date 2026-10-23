//! The process entry point as a library function.
//!
//! Keeping the whole path (arguments, diagnostics, mode, provider, runtime,
//! output) behind one function lets tests drive the same code the executable
//! runs, with the environment and the provider endpoint injected.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nyan_coding_agent_session::NativeSession;

use nyan_coding_agent_core::{CoreError, VERSION};
use nyan_coding_agent_protocol::{RpcCommandName, SessionEventName};
use nyan_coding_agent_provider::{ModelProvider, ProviderModel, ProviderRegistry};
use nyan_coding_agent_provider_http::OpenAiChatProvider;
use nyan_coding_agent_tools::builtin::BuiltinTools;

use crate::app_mode::{AppMode, resolve_app_mode};
use crate::args::{Args, DiagnosticKind, parse_args};
use crate::headless::{HeadlessMode, HeadlessOptions, SharedWriter, run_headless};
use crate::session_setup::open_session;
use crate::system_prompt::{
    PromptOptions, build_system_prompt, load_project_context_files, text_or_file,
};
use crate::tool_selection::select_tools;

/// The `OpenAI` endpoint used when nothing else is configured.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Looks up an environment variable.
pub type EnvLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

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
    /// Base URL of the OpenAI-compatible endpoint.
    pub openai_base_url: String,
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Environment")
            .field("stdin_is_tty", &self.stdin_is_tty)
            .field("stdout_is_tty", &self.stdout_is_tty)
            .field("cwd", &self.cwd)
            .field("openai_base_url", &self.openai_base_url)
            .finish_non_exhaustive()
    }
}

struct OneProvider(OpenAiChatProvider);

impl ProviderRegistry for OneProvider {
    fn provider(&self, id: &str) -> Result<&dyn ModelProvider, CoreError> {
        if id == self.0.id() {
            Ok(&self.0)
        } else {
            Err(CoreError::Unsupported {
                operation: format!("provider {id}"),
            })
        }
    }
}

fn help_text() -> String {
    format!(
        "nyan-coding-agent {VERSION}\n\n\
         Rust Coding Agent preview. It is not a usable replacement yet.\n\n\
         Protocol contracts frozen: {} RPC commands, {} session events.\n\n\
         Supported now: --print and --mode json with --provider openai and\
         the read, bash, edit, write, grep, find, and ls tools\
         (--tools, --exclude-tools, --no-tools).\n\n\
         Options:\n  -h, --help       Show this help\n  -v, --version    Show the version\n",
        RpcCommandName::all().len(),
        SessionEventName::all().len()
    )
}

fn write_to(writer: &SharedWriter, text: &str) {
    if let Ok(mut writer) = writer.lock() {
        let _ = writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.flush());
    }
}

/// Splits `provider/model` when no `--provider` was given.
fn resolve_model(args: &Args) -> Result<ProviderModel, String> {
    let Some(model) = args.model.as_deref() else {
        return Err("--model is required: the Rust preview has no default model".to_owned());
    };
    match args.provider.as_deref() {
        Some(provider) => Ok(ProviderModel {
            provider: provider.to_owned(),
            model: model.to_owned(),
        }),
        None => model
            .split_once('/')
            .filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
            .map(|(provider, id)| ProviderModel {
                provider: provider.to_owned(),
                model: id.to_owned(),
            })
            .ok_or_else(|| {
                "--model needs a provider: pass --provider or use \"provider/model\"".to_owned()
            }),
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
        api_key: String,
        messages: Vec<String>,
        tools: Vec<&'static str>,
        system_prompt: String,
        session: NativeSession,
    },
}

/// The agent directory: `NYAN_CODING_AGENT_DIR`, else `~/.nyan/agent`.
fn agent_dir(env: &Environment) -> PathBuf {
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
    if !parsed.file_args.is_empty() {
        return fail(
            err,
            "@file arguments are not supported by the Rust preview yet",
        );
    }
    let model = match resolve_model(&parsed) {
        Ok(model) => model,
        Err(message) => return fail(err, &message),
    };
    if model.provider != "openai" {
        return fail(
            err,
            &format!(
                "provider \"{}\" is not supported by the Rust preview yet (supported: openai)",
                model.provider
            ),
        );
    }
    let Some(api_key) = parsed
        .api_key
        .clone()
        .or_else(|| (env.var)("OPENAI_API_KEY").filter(|key| !key.is_empty()))
    else {
        return fail(
            err,
            "no API key for provider \"openai\": set OPENAI_API_KEY or pass --api-key",
        );
    };
    let tools = match select_tools(&parsed) {
        Ok(tools) => tools,
        Err(message) => return fail(err, &message),
    };
    let system_prompt = build_prompt(&parsed, &tools, env);
    let session = match open_session(&parsed, &agent_dir(env), &env.cwd) {
        Ok(session) => session,
        Err(message) => return fail(err, &message),
    };
    Plan::Run {
        mode,
        model,
        api_key,
        messages: parsed.messages,
        tools,
        system_prompt,
        session,
    }
}

/// Runs the process for `argv` (without the program name) and returns its exit code.
#[must_use]
pub fn run(argv: &[String], env: &Environment, out: &SharedWriter, err: &SharedWriter) -> i32 {
    let (mode, model, api_key, messages, tools, system_prompt, session) =
        match plan(argv, env, out, err) {
            Plan::Exit(code) => return code,
            Plan::Run {
                mode,
                model,
                api_key,
                messages,
                tools,
                system_prompt,
                session,
            } => (
                mode,
                model,
                api_key,
                messages,
                tools,
                system_prompt,
                session,
            ),
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
    let provider = match OpenAiChatProvider::new(
        "openai",
        env.openai_base_url.clone(),
        Some(api_key),
        runtime.handle().clone(),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            write_to(err, &format!("Error: {error}\n"));
            return 1;
        }
    };
    let providers = OneProvider(provider);
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
