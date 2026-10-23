//! Minimal native executable boundary for the Rust Coding Agent preview.

use nyan_coding_agent_cli::args::{DiagnosticKind, parse_args};
use nyan_coding_agent_core::VERSION;
use nyan_coding_agent_protocol::{RpcCommandName, SessionEventName};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let parsed = parse_args(&arguments);

    let mut failed = false;
    for diagnostic in &parsed.diagnostics {
        match diagnostic.kind {
            DiagnosticKind::Error => {
                failed = true;
                eprintln!("Error: {}", diagnostic.message);
            }
            DiagnosticKind::Warning => eprintln!("Warning: {}", diagnostic.message),
        }
    }
    if failed {
        std::process::exit(1);
    }

    if parsed.version {
        println!("{VERSION}");
        return;
    }
    if parsed.help || arguments.is_empty() {
        print_help();
        return;
    }

    eprintln!(
        "nyan-coding-agent: this Rust preview does not run prompts yet; use the TypeScript `nyan` command"
    );
    std::process::exit(2);
}

fn print_help() {
    println!("nyan-coding-agent {VERSION}\n");
    println!("Rust Coding Agent preview. It is not a usable replacement yet.");
    println!(
        "\nProtocol contracts frozen: {} RPC commands, {} session events.",
        RpcCommandName::all().len(),
        SessionEventName::all().len()
    );
    println!("\nOptions:\n  -h, --help       Show this help\n  -v, --version    Show the version");
}
