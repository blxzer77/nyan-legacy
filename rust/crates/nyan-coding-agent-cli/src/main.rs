//! Native executable for the Rust Coding Agent preview.

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};

use nyan_coding_agent_cli::headless::SharedWriter;
use nyan_coding_agent_cli::run::{Environment, run};

/// Piped stdin, trimmed; nothing when stdin is a terminal or empty.
fn read_piped_stdin() -> Option<String> {
    use std::io::Read;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Asks one question on stderr and reads the answer line from stdin.
///
/// A `secret` answer on a terminal is read without echo.
fn prompt_user(message: &str, secret: bool) -> Option<String> {
    use std::io::{BufRead, Write};
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(message.as_bytes());
    let _ = stderr.flush();
    if secret && std::io::stdin().is_terminal() {
        let answer = read_hidden_line();
        let _ = stderr.write_all(b"\n");
        return answer;
    }
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

/// Reads one line from the terminal in raw mode, so nothing is echoed.
fn read_hidden_line() -> Option<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    crossterm::terminal::enable_raw_mode().ok()?;
    let mut answer = String::new();
    let result = loop {
        let Ok(Event::Key(key)) = read() else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Enter => break Some(answer),
            KeyCode::Backspace => {
                answer.pop();
            }
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                break None;
            }
            KeyCode::Char(character) => answer.push(character),
            _ => {}
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

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
        base_urls: std::collections::BTreeMap::new(),
        read_stdin: Box::new(read_piped_stdin),
        prompt: Box::new(prompt_user),
    };
    let out: SharedWriter = Arc::new(Mutex::new(std::io::stdout()));
    let err: SharedWriter = Arc::new(Mutex::new(std::io::stderr()));
    std::process::exit(run(&argv, &environment, &out, &err));
}
