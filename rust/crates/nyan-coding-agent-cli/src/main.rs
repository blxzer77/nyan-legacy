//! Native executable for the Rust Coding Agent preview.

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};

use nyan_coding_agent_cli::headless::SharedWriter;
use nyan_coding_agent_cli::run::{Environment, OPENAI_BASE_URL, run};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("Error: cannot determine the working directory: {error}");
            std::process::exit(1);
        }
    };
    let environment = Environment {
        var: Box::new(|name| std::env::var(name).ok()),
        stdin_is_tty: std::io::stdin().is_terminal(),
        stdout_is_tty: std::io::stdout().is_terminal(),
        cwd,
        openai_base_url: OPENAI_BASE_URL.to_owned(),
    };
    let out: SharedWriter = Arc::new(Mutex::new(std::io::stdout()));
    let err: SharedWriter = Arc::new(Mutex::new(std::io::stderr()));
    std::process::exit(run(&argv, &environment, &out, &err));
}
