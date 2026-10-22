//! Minimal native executable boundary for RS1.

use nyan_coding_agent_core::VERSION;
use nyan_coding_agent_protocol::{RpcCommandName, SessionEventName};

fn main() {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("--version" | "-v") => println!("{VERSION}"),
        Some("--help" | "-h") | None => print_help(),
        Some(argument) => {
            eprintln!(
                "nyan-coding-agent: Rust runtime scaffold; unsupported command-line argument: {argument}"
            );
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!("nyan-coding-agent {VERSION}\n");
    println!("RS1 workspace scaffold. The Rust Coding Agent is not a usable replacement yet.");
    println!(
        "\nProtocol contracts frozen: {} RPC commands, {} session events.",
        RpcCommandName::all().len(),
        SessionEventName::all().len()
    );
    println!("\nOptions:\n  -h, --help       Show this help\n  -v, --version    Show the version");
}
