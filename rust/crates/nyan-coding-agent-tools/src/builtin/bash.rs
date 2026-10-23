//! The `bash` tool, ported from `core/tools/bash.ts`.
//!
//! The command runs in its own process group, so cancelling or timing out ends
//! every child it started. Output is streamed to the caller as it arrives and
//! only the last 2000 lines or 50 KB are returned; when the output is larger, the
//! complete output is saved to a temporary file whose path is part of the result.
//!
//! Parity gaps: the TypeScript tool adds `NYAN_*` variables to the child
//! environment and supports a configured shell path and detached-child tracking;
//! none of that exists here. The Windows shell lookup (Git Bash locations, then
//! `bash.exe` on `PATH`) is ported but has not been run on Windows. Cancellation
//! is reported as an unknown outcome rather than as "Command aborted".

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nyan_coding_agent_core::{BoxFuture, ExecutionContext, ToolResult};

use super::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, Truncation, format_size, truncate_tail,
};
use super::{optional_number, parse_arguments, required_string};
use crate::{OutputSink, ProcessSpec, Tool, ToolDefinition, ToolError, run_process};

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);
/// How much of the end of the output stays in memory. Only the last
/// `DEFAULT_MAX_BYTES` can ever be returned, so this leaves room to spare.
const TAIL_KEEP_BYTES: usize = DEFAULT_MAX_BYTES * 4;

const SCHEMA: &str = r#"{"type":"object","properties":{"command":{"type":"string","description":"Shell command to execute"},"timeout":{"type":"number","description":"Timeout in seconds (optional, no default timeout)"}},"required":["command"]}"#;

/// The `bash` tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct BashTool;

impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_owned(),
            description: format!(
                "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters_schema: SCHEMA.to_owned(),
        }
    }

    fn execute(
        &self,
        input: String,
        context: ExecutionContext,
    ) -> BoxFuture<Result<ToolResult, ToolError>> {
        Box::pin(async move {
            let arguments = parse_arguments(&input)?;
            let command = required_string(&arguments, "command")?.to_owned();
            let timeout = resolve_timeout(optional_number(&arguments, "timeout")?)?;
            execute(command, timeout, context).await
        })
    }
}

/// Validates the timeout the way the TypeScript tool does.
fn resolve_timeout(timeout: Option<f64>) -> Result<Option<(f64, Duration)>, ToolError> {
    let Some(seconds) = timeout else {
        return Ok(None);
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(ToolError::InvalidInput {
            message: "Invalid timeout: must be a finite number of seconds".to_owned(),
        });
    }
    if seconds * 1000.0 > MAX_TIMEOUT_MS {
        return Err(ToolError::InvalidInput {
            message: format!(
                "Invalid timeout: maximum is {} seconds",
                MAX_TIMEOUT_MS / 1000.0
            ),
        });
    }
    Ok(Some((seconds, Duration::from_secs_f64(seconds))))
}

/// The shell program and the arguments that precede the command text.
fn shell() -> Result<(PathBuf, Vec<String>), ToolError> {
    if cfg!(windows) {
        let mut searched = Vec::new();
        for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
                let candidate = PathBuf::from(root).join("Git").join("bin").join("bash.exe");
                if candidate.exists() {
                    return Ok((candidate, vec!["-c".to_owned()]));
                }
                searched.push(candidate);
            }
        }
        if let Some(found) = find_on_path("bash.exe") {
            return Ok((found, vec!["-c".to_owned()]));
        }
        let listing = searched
            .iter()
            .map(|path| format!("  {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ToolError::Execution {
            message: format!(
                "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{listing}"
            ),
        });
    }
    if Path::new("/bin/bash").exists() {
        return Ok((PathBuf::from("/bin/bash"), vec!["-c".to_owned()]));
    }
    if let Some(found) = find_on_path("bash") {
        return Ok((found, vec!["-c".to_owned()]));
    }
    Ok((PathBuf::from("sh"), vec!["-c".to_owned()]))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// Collects process output: the tail in memory, the whole of it on disk once it
/// no longer fits the limits.
struct Accumulator {
    tail: Vec<u8>,
    total_bytes: usize,
    newlines: usize,
    ends_with_newline: bool,
    spill: Option<(PathBuf, File)>,
    spill_failed: bool,
}

impl Accumulator {
    fn new() -> Self {
        Self {
            tail: Vec::new(),
            total_bytes: 0,
            newlines: 0,
            ends_with_newline: true,
            spill: None,
            spill_failed: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.total_bytes += chunk.len();
        self.newlines += chunk.split(|byte| *byte == b'\n').count() - 1;
        self.ends_with_newline = chunk.last() == Some(&b'\n');
        // Save the whole output as soon as either limit is exceeded, because the
        // returned text will then be a truncated tail.
        let exceeds_limits =
            self.total_bytes > DEFAULT_MAX_BYTES || self.total_lines() > DEFAULT_MAX_LINES;
        if exceeds_limits && self.spill.is_none() && !self.spill_failed {
            self.open_spill();
        }
        if let Some((_, file)) = &mut self.spill
            && file.write_all(chunk).is_err()
        {
            self.spill = None;
            self.spill_failed = true;
        }
        self.tail.extend_from_slice(chunk);
        if self.tail.len() > TAIL_KEEP_BYTES {
            let excess = self.tail.len() - TAIL_KEEP_BYTES;
            self.tail.drain(..excess);
        }
    }

    /// Creates the spill file and writes everything seen so far into it.
    fn open_spill(&mut self) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path =
            std::env::temp_dir().join(format!("nyan-bash-{}-{nonce}.log", std::process::id()));
        // Everything before this chunk is still in `tail`: the spill opens as soon as
        // a limit is exceeded, long before the tail starts discarding.
        match File::create(&path).and_then(|mut file| {
            file.write_all(&self.tail)?;
            Ok(file)
        }) {
            Ok(file) => self.spill = Some((path, file)),
            Err(_) => self.spill_failed = true,
        }
    }

    fn total_lines(&self) -> usize {
        if self.total_bytes == 0 {
            0
        } else if self.ends_with_newline {
            self.newlines
        } else {
            self.newlines + 1
        }
    }

    fn snapshot(&self) -> (Truncation, Option<PathBuf>) {
        // The tail can start in the middle of a character; skip continuation bytes.
        let start = self
            .tail
            .iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(0);
        let text = String::from_utf8_lossy(&self.tail[start..]).into_owned();
        let mut truncation = truncate_tail(&text, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
        let truncated =
            self.total_bytes > DEFAULT_MAX_BYTES || self.total_lines() > DEFAULT_MAX_LINES;
        if truncated {
            truncation.truncated = true;
            truncation.total_lines = self.total_lines();
            truncation.total_bytes = self.total_bytes;
        }
        (
            truncation,
            self.spill.as_ref().map(|(path, _)| path.clone()),
        )
    }
}

fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_owned()
    } else {
        format!("{text}\n\n{status}")
    }
}

/// The model-facing text for a snapshot, with the truncation notice appended.
fn format_output(
    truncation: &Truncation,
    full_output: Option<&Path>,
    last_line_bytes: usize,
    empty_text: &str,
) -> String {
    let mut text = if truncation.content.is_empty() {
        empty_text.to_owned()
    } else {
        truncation.content.clone()
    };
    if !truncation.truncated {
        return text;
    }
    let start_line = truncation
        .total_lines
        .saturating_sub(truncation.output_lines)
        + 1;
    let end_line = truncation.total_lines;
    let location = full_output.map_or_else(String::new, |path| {
        format!(" Full output: {}", path.display())
    });
    let notice = if truncation.last_line_partial {
        format!(
            "[Showing last {} of line {end_line} (line is {}).{location}]",
            format_size(truncation.output_bytes),
            format_size(last_line_bytes),
        )
    } else if truncation.truncated_by == Some(super::truncate::TruncatedBy::Lines) {
        format!(
            "[Showing lines {start_line}-{end_line} of {}.{location}]",
            truncation.total_lines
        )
    } else {
        format!(
            "[Showing lines {start_line}-{end_line} of {} ({} limit).{location}]",
            truncation.total_lines,
            format_size(DEFAULT_MAX_BYTES)
        )
    };
    text.push_str("\n\n");
    text.push_str(&notice);
    text
}

async fn execute(
    command: String,
    timeout: Option<(f64, Duration)>,
    context: ExecutionContext,
) -> Result<ToolResult, ToolError> {
    let (program, mut arguments) = shell()?;
    arguments.push(command);
    let accumulator = Arc::new(Mutex::new(Accumulator::new()));
    let last_update: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let sink: OutputSink = {
        let accumulator = Arc::clone(&accumulator);
        let last_update = Arc::clone(&last_update);
        let context = context.clone();
        Arc::new(move |chunk| {
            let Ok(mut accumulator) = accumulator.lock() else {
                return;
            };
            accumulator.push(chunk);
            // The first chunk is reported at once; later ones at most every interval.
            let due = last_update.lock().is_ok_and(|mut last| {
                let due = last.is_none_or(|at| at.elapsed() >= UPDATE_INTERVAL);
                if due {
                    *last = Some(Instant::now());
                }
                due
            });
            if due {
                let (truncation, path) = accumulator.snapshot();
                let text = format_output(&truncation, path.as_deref(), 0, "");
                drop(accumulator);
                context.report_update(ToolResult {
                    text,
                    is_error: false,
                });
            }
        })
    };

    let mut spec =
        ProcessSpec::new(program.display().to_string(), context.cwd.clone()).with_output_sink(sink);
    spec.args = arguments;
    if let Some((_, duration)) = timeout {
        spec = spec.with_timeout(duration);
    }
    let outcome = run_process(spec, context.cancellation.clone()).await?;

    let (truncation, full_output) = accumulator
        .lock()
        .map_err(|_| ToolError::Execution {
            message: "bash output buffer was poisoned".to_owned(),
        })?
        .snapshot();
    let last_line_bytes = truncation.output_bytes;
    if outcome.timed_out {
        let text = format_output(&truncation, full_output.as_deref(), last_line_bytes, "");
        let seconds = timeout.map_or(0.0, |(seconds, _)| seconds);
        return Err(ToolError::Execution {
            message: append_status(&text, &format!("Command timed out after {seconds} seconds")),
        });
    }
    let text = format_output(
        &truncation,
        full_output.as_deref(),
        last_line_bytes,
        "(no output)",
    );
    match outcome.status_code {
        None => Err(ToolError::Execution {
            message: append_status(&text, "Command terminated without an exit code"),
        }),
        Some(0) => Ok(ToolResult {
            text,
            is_error: false,
        }),
        Some(code) => Ok(ToolResult {
            text: append_status(&text, &format!("Command exited with code {code}")),
            is_error: true,
        }),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;

    use nyan_coding_agent_core::{CancellationToken, ExecutionContext, ToolResult};
    use serde_json::{Value, json};

    use super::BashTool;
    use crate::Tool;
    use crate::builtin::test_support::TempDir;

    fn block_on<F: Future>(future: F) -> F::Output {
        struct Parker(thread::Thread);
        impl Wake for Parker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(Parker(thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match Pin::new(&mut future).poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => thread::park(),
            }
        }
    }

    fn run(
        dir: &TempDir,
        arguments: &Value,
    ) -> (Result<ToolResult, String>, Vec<String>, CancellationToken) {
        let cancellation = CancellationToken::new();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&updates);
        let context = ExecutionContext::new(dir.path(), cancellation.clone()).with_update_sink(
            Arc::new(move |result: ToolResult| {
                sink.lock().expect("updates lock").push(result.text);
            }),
        );
        let result = block_on(BashTool.execute(arguments.to_string(), context))
            .map_err(|error| error.to_string());
        let updates = updates.lock().expect("updates lock").clone();
        (result, updates, cancellation)
    }

    #[test]
    fn stdout_and_stderr_are_both_returned() {
        let dir = TempDir::new("bash-basic");
        let (result, _, _) = run(&dir, &json!({"command": "echo out; echo err >&2"}));
        let text = result.expect("run").text;
        assert!(text.contains("out") && text.contains("err"), "{text}");
    }

    #[test]
    fn the_command_runs_in_the_working_directory() {
        let dir = TempDir::new("bash-cwd");
        std::fs::write(dir.path().join("marker.txt"), "x").expect("write");
        let (result, _, _) = run(&dir, &json!({"command": "ls"}));
        assert_eq!(result.expect("run").text, "marker.txt\n");
    }

    #[test]
    fn silent_success_says_so() {
        let dir = TempDir::new("bash-silent");
        let (result, _, _) = run(&dir, &json!({"command": "true"}));
        assert_eq!(result.expect("run").text, "(no output)");
    }

    #[test]
    fn a_non_zero_exit_is_an_error_result_that_keeps_the_output() {
        let dir = TempDir::new("bash-exit");
        let (result, _, _) = run(&dir, &json!({"command": "echo partial; exit 3"}));
        let result = result.expect("a failing command is a result, not an execution error");
        assert!(result.is_error);
        assert_eq!(result.text, "partial\n\n\nCommand exited with code 3");
        let (result, _, _) = run(&dir, &json!({"command": "exit 1"}));
        let result = result.expect("run");
        assert_eq!(result.text, "(no output)\n\nCommand exited with code 1");
    }

    #[test]
    fn a_timeout_reports_the_output_so_far_and_ends_the_whole_group() {
        let dir = TempDir::new("bash-timeout");
        let started = std::time::Instant::now();
        let (result, _, _) = run(
            &dir,
            &json!({"command": "echo started; sleep 30 & sleep 30", "timeout": 0.3}),
        );
        assert_eq!(
            result.expect_err("timeout"),
            "started\n\n\nCommand timed out after 0.3 seconds"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn invalid_timeouts_are_rejected_before_anything_runs() {
        let dir = TempDir::new("bash-bad-timeout");
        for (timeout, message) in [
            (
                json!(0),
                "Invalid timeout: must be a finite number of seconds",
            ),
            (
                json!(-5),
                "Invalid timeout: must be a finite number of seconds",
            ),
            (
                json!(3_000_000),
                "Invalid timeout: maximum is 2147483.647 seconds",
            ),
        ] {
            let (result, _, _) = run(&dir, &json!({"command": "touch ran", "timeout": timeout}));
            assert_eq!(result.expect_err("invalid timeout"), message);
            assert!(!dir.path().join("ran").exists());
        }
    }

    #[test]
    fn large_output_keeps_the_tail_and_saves_the_whole_to_a_file() {
        let dir = TempDir::new("bash-large");
        let (result, _, _) = run(&dir, &json!({"command": "seq 1 5000"}));
        let text = result.expect("run").text;
        assert!(text.starts_with("3001\n3002\n"), "{}", &text[..40]);
        assert!(text.contains("5000\n\n[Showing lines 3001-5000 of 5000. Full output: "));
        let path = text
            .rsplit("Full output: ")
            .next()
            .and_then(|rest| rest.strip_suffix(']'))
            .expect("full output path");
        let full = std::fs::read_to_string(path).expect("spill file");
        assert_eq!(full.lines().count(), 5000);
        assert!(full.starts_with("1\n2\n") && full.ends_with("4999\n5000\n"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn long_running_output_is_streamed_before_the_command_finishes() {
        let dir = TempDir::new("bash-stream");
        let (result, updates, _) = run(
            &dir,
            &json!({"command": "echo first; sleep 0.4; echo second"}),
        );
        assert_eq!(result.expect("run").text, "first\nsecond\n");
        assert!(
            updates
                .iter()
                .any(|text| text.contains("first") && !text.contains("second")),
            "an update must show the early output alone: {updates:?}"
        );
    }

    #[test]
    fn cancellation_ends_the_command_and_is_reported_as_an_unknown_outcome() {
        let dir = TempDir::new("bash-cancel");
        let cancellation = CancellationToken::new();
        let context = ExecutionContext::new(dir.path(), cancellation.clone());
        let canceller = {
            let cancellation = cancellation.clone();
            thread::spawn(move || {
                thread::sleep(std::time::Duration::from_millis(200));
                cancellation.cancel();
            })
        };
        let started = std::time::Instant::now();
        let result = block_on(BashTool.execute(
            json!({"command": "sleep 30 & sleep 30"}).to_string(),
            context,
        ));
        canceller.join().expect("canceller");
        assert!(matches!(
            result,
            Err(crate::ToolError::UnknownOutcome { .. })
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}
