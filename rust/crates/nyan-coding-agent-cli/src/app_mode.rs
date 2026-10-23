//! Application-mode resolution, ported from `resolveAppMode` in `main.ts`.

use crate::args::{Args, Mode};

/// How the process talks to its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppMode {
    /// Bidirectional JSONL protocol.
    Rpc,
    /// Strict JSONL event stream for one invocation.
    Json,
    /// Plain text answer, then exit.
    Print,
    /// Terminal UI.
    Interactive,
}

/// Picks the mode from the flags and whether stdin and stdout are terminals.
///
/// `--mode rpc` and `--mode json` win. Otherwise `--print`, or either stream
/// being redirected, selects print mode. `--mode text` has no effect of its own.
#[must_use]
pub fn resolve_app_mode(args: &Args, stdin_is_tty: bool, stdout_is_tty: bool) -> AppMode {
    match args.mode {
        Some(Mode::Rpc) => AppMode::Rpc,
        Some(Mode::Json) => AppMode::Json,
        Some(Mode::Text) | None => {
            if args.print || !stdin_is_tty || !stdout_is_tty {
                AppMode::Print
            } else {
                AppMode::Interactive
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppMode, resolve_app_mode};
    use crate::args::parse_args;

    fn mode(args: &[&str], stdin_tty: bool, stdout_tty: bool) -> AppMode {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        resolve_app_mode(&parse_args(&args), stdin_tty, stdout_tty)
    }

    #[test]
    fn explicit_protocol_modes_win_over_terminal_state() {
        assert_eq!(mode(&["--mode", "rpc"], true, true), AppMode::Rpc);
        assert_eq!(mode(&["--mode", "json"], true, true), AppMode::Json);
        assert_eq!(mode(&["--mode", "json", "-p"], false, false), AppMode::Json);
    }

    #[test]
    fn terminals_get_the_interactive_ui() {
        assert_eq!(mode(&[], true, true), AppMode::Interactive);
        assert_eq!(mode(&["--mode", "text"], true, true), AppMode::Interactive);
    }

    #[test]
    fn print_or_a_redirected_stream_selects_print() {
        assert_eq!(mode(&["-p", "hi"], true, true), AppMode::Print);
        assert_eq!(mode(&[], false, true), AppMode::Print);
        assert_eq!(mode(&[], true, false), AppMode::Print);
    }
}
