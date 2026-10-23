//! Tool registration, execution, and direct subprocess boundaries.

pub mod builtin;

use std::future::Future;
use std::io::Read;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use command_group::{CommandGroup, GroupChild};
use nyan_coding_agent_core::{BoxFuture, CancellationToken, ExecutionContext, ToolResult};

/// A model-visible tool definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    /// Stable tool name.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// JSON Schema of the arguments, as JSON text. Empty means an object with no properties.
    pub parameters_schema: String,
}

/// Errors from tool admission or execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolError {
    /// The tool name was not registered.
    UnknownTool {
        /// The missing tool name.
        name: String,
    },
    /// The input did not match the tool contract.
    InvalidInput {
        /// Validation diagnostic.
        message: String,
    },
    /// The tool process or implementation failed.
    Execution {
        /// Execution diagnostic.
        message: String,
    },
    /// The caller cancelled the tool before its external effect started.
    Cancelled,
    /// The tool started an external effect, but cancellation prevented proving its outcome.
    UnknownOutcome {
        /// The operation whose outcome is not known.
        operation: String,
        /// Diagnostic explaining the uncertainty.
        message: String,
    },
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool { name } => write!(formatter, "unknown tool: {name}"),
            Self::InvalidInput { message } | Self::Execution { message } => {
                formatter.write_str(message)
            }
            Self::Cancelled => formatter.write_str("tool execution cancelled"),
            Self::UnknownOutcome { operation, message } => {
                write!(
                    formatter,
                    "tool operation {operation} has unknown outcome: {message}"
                )
            }
        }
    }
}

impl std::error::Error for ToolError {}

/// How long output readers may keep running after the process leader has exited.
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Receives output bytes from a running process as they arrive, on the reader threads.
pub type OutputSink = Arc<dyn Fn(&[u8]) + Send + Sync + 'static>;

/// A managed subprocess specification.
#[derive(Clone)]
pub struct ProcessSpec {
    /// Executable name or path.
    pub program: String,
    /// Arguments in operating-system order.
    pub args: Vec<String>,
    /// Working directory for the child.
    pub cwd: PathBuf,
    /// Kill the process group after this long.
    pub timeout: Option<Duration>,
    /// When set, output is streamed here and not captured in [`ProcessOutput`].
    pub on_output: Option<OutputSink>,
}

impl std::fmt::Debug for ProcessSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .field("streaming", &self.on_output.is_some())
            .finish()
    }
}

impl ProcessSpec {
    /// Creates a process specification.
    #[must_use]
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            timeout: None,
            on_output: None,
        }
    }

    /// Appends one argument and returns the updated specification.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.args.push(argument.into());
        self
    }

    /// Kills the process group if it runs longer than `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Streams output to `sink` instead of capturing it.
    #[must_use]
    pub fn with_output_sink(mut self, sink: OutputSink) -> Self {
        self.on_output = Some(sink);
        self
    }
}

/// Captured result of a managed process group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    /// Exit code, or `None` when the process terminated by signal or timed out.
    pub status_code: Option<i32>,
    /// Whether the process group was killed because it exceeded its timeout.
    pub timed_out: bool,
    /// Complete captured stdout (empty when streaming).
    pub stdout: String,
    /// Complete captured stderr (empty when streaming).
    pub stderr: String,
}

/// Runs one subprocess group and owns it until exit, timeout, or cancellation.
///
/// The returned future does not choose an async executor. The process group is
/// polled by a dedicated standard-library thread, and cancellation kills the
/// group before the future resolves. Dropping the future also requests cancellation.
/// A timeout kills the group too, but keeps the output produced so far and
/// reports it through [`ProcessOutput::timed_out`].
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when spawning or capturing output fails,
/// [`ToolError::Cancelled`] when cancellation happens before spawn, and
/// [`ToolError::UnknownOutcome`] when a running process group is terminated by
/// cancellation or its completion cannot be proven.
pub fn run_process(
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> BoxFuture<Result<ProcessOutput, ToolError>> {
    let (sender, receiver) = mpsc::channel();
    let state = Arc::new(ProcessFutureState {
        done: AtomicBool::new(false),
        waker: Mutex::new(None),
        dropped: AtomicBool::new(false),
    });
    let thread_state = Arc::clone(&state);
    thread::spawn(move || {
        let result = execute_process(spec, &cancellation, &thread_state.dropped);
        let _ = sender.send(result);
        thread_state.done.store(true, Ordering::Release);
        if let Ok(mut waker) = thread_state.waker.lock()
            && let Some(waker) = waker.take()
        {
            waker.wake();
        }
    });
    Box::pin(ProcessFuture { receiver, state })
}

struct ProcessFutureState {
    done: AtomicBool,
    waker: Mutex<Option<Waker>>,
    /// Set when the caller dropped the future. It ends this process group only:
    /// the caller's cancellation token belongs to the whole run and must not be touched.
    dropped: AtomicBool,
}

struct ProcessFuture {
    receiver: Receiver<Result<ProcessOutput, ToolError>>,
    state: Arc<ProcessFutureState>,
}

impl Drop for ProcessFuture {
    fn drop(&mut self) {
        self.state.dropped.store(true, Ordering::Release);
    }
}

impl Future for ProcessFuture {
    type Output = Result<ProcessOutput, ToolError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.receiver.try_recv() {
            Ok(result) => Poll::Ready(result),
            Err(TryRecvError::Disconnected) => Poll::Ready(Err(ToolError::Execution {
                message: "process worker disconnected before returning a result".to_owned(),
            })),
            Err(TryRecvError::Empty) => {
                if let Ok(mut waker) = this.state.waker.lock() {
                    *waker = Some(context.waker().clone());
                }
                if this.state.done.load(Ordering::Acquire) {
                    context.waker().wake_by_ref();
                }
                Poll::Pending
            }
        }
    }
}

// The worker owns the spec so the caller can return immediately while the child is alive.
#[allow(clippy::needless_pass_by_value)]
fn execute_process(
    spec: ProcessSpec,
    cancellation: &CancellationToken,
    dropped: &AtomicBool,
) -> Result<ProcessOutput, ToolError> {
    let stop_requested = || cancellation.is_cancelled() || dropped.load(Ordering::Acquire);
    if stop_requested() {
        return Err(ToolError::Cancelled);
    }
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child: GroupChild = command
        .group_spawn()
        .map_err(|error| ToolError::Execution {
            message: format!(
                "failed to spawn process group for {}: {error}",
                spec.program
            ),
        })?;
    let Some(mut stdout) = child.inner().stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ToolError::UnknownOutcome {
            operation: spec.program.clone(),
            message: "child stdout pipe was not available after spawn".to_owned(),
        });
    };
    let Some(mut stderr) = child.inner().stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ToolError::UnknownOutcome {
            operation: spec.program.clone(),
            message: "child stderr pipe was not available after spawn".to_owned(),
        });
    };
    let stdout_sink = spec.on_output.clone();
    let stderr_sink = spec.on_output.clone();
    let stdout_reader = thread::spawn(move || read_pipe(&mut stdout, stdout_sink.as_ref()));
    let stderr_reader = thread::spawn(move || read_pipe(&mut stderr, stderr_sink.as_ref()));

    let deadline = spec.timeout.map(|timeout| Instant::now() + timeout);
    let mut timed_out = false;
    let status = loop {
        if stop_requested() {
            let _ = child.kill();
            let _ = child.wait();
            discard_pipe(stdout_reader);
            discard_pipe(stderr_reader);
            return Err(ToolError::UnknownOutcome {
                operation: spec.program.clone(),
                message: "process group was terminated before completion".to_owned(),
            });
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                discard_pipe(stdout_reader);
                discard_pipe(stderr_reader);
                return Err(ToolError::UnknownOutcome {
                    operation: spec.program.clone(),
                    message: format!("failed to wait for process group: {error}"),
                });
            }
        }
    };
    // A background grandchild can keep the pipes open after the leader exits. Give the
    // readers a short grace period, then end the whole group so they reach EOF.
    let grace_deadline = Instant::now() + PIPE_DRAIN_GRACE;
    while !(stdout_reader.is_finished() && stderr_reader.is_finished()) {
        if Instant::now() >= grace_deadline {
            let _ = child.kill();
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let stdout = join_pipe(stdout_reader)?;
    let stderr = join_pipe(stderr_reader)?;
    Ok(ProcessOutput {
        status_code: if timed_out {
            None
        } else {
            status.and_then(|status| status.code())
        },
        timed_out,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Reads a pipe to its end, streaming chunks to `sink` or capturing them.
fn read_pipe(reader: &mut impl Read, sink: Option<&OutputSink>) -> Result<Vec<u8>, ToolError> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(captured),
            Ok(count) => match sink {
                Some(sink) => sink(&buffer[..count]),
                None => captured.extend_from_slice(&buffer[..count]),
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(ToolError::Execution {
                    message: format!("failed to read child output: {error}"),
                });
            }
        }
    }
}

fn join_pipe(reader: thread::JoinHandle<Result<Vec<u8>, ToolError>>) -> Result<Vec<u8>, ToolError> {
    reader.join().map_err(|_| ToolError::Execution {
        message: "child output reader panicked".to_owned(),
    })?
}

fn discard_pipe(reader: thread::JoinHandle<Result<Vec<u8>, ToolError>>) {
    let _ = reader.join();
}

/// A tool implementation owned by the runtime.
pub trait Tool: Send + Sync {
    /// Returns the model-visible definition.
    fn definition(&self) -> ToolDefinition;

    /// Executes a validated call.
    fn execute(
        &self,
        input: String,
        context: ExecutionContext,
    ) -> BoxFuture<Result<ToolResult, ToolError>>;
}

/// A registry of tools available to one agent runtime.
pub trait ToolRegistry: Send + Sync {
    /// Resolves a tool by name.
    fn tool(&self, name: &str) -> Option<&dyn Tool>;

    /// Returns definitions in declaration order.
    fn definitions(&self) -> Vec<ToolDefinition>;
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;
    use std::time::Duration;

    use nyan_coding_agent_core::CancellationToken;

    use super::{ProcessSpec, ToolDefinition, ToolError, run_process};

    #[test]
    fn definitions_are_stable_values() {
        let definition = ToolDefinition {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters_schema: String::new(),
        };
        assert_eq!(definition.name, "read");
    }

    #[test]
    fn captures_direct_process_output() {
        let spec = if cfg!(windows) {
            ProcessSpec::new("cmd", ".").arg("/C").arg("echo hello")
        } else {
            ProcessSpec::new("printf", ".").arg("hello")
        };
        let output = block_on(run_process(spec, CancellationToken::new())).expect("process output");

        assert_eq!(output.status_code, Some(0));
        assert_eq!(output.stdout.trim(), "hello");
        assert_eq!(output.stderr, "");
    }

    #[test]
    fn cancellation_kills_process_group() {
        let spec = if cfg!(windows) {
            ProcessSpec::new("cmd", ".")
                .arg("/C")
                .arg("ping 127.0.0.1 -n 10 & ping 127.0.0.1 -n 10")
        } else {
            ProcessSpec::new("sh", ".").arg("-c").arg("sleep 10 & wait")
        };
        let cancellation = CancellationToken::new();
        let cancellation_for_thread = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancellation_for_thread.cancel();
        });
        let result = block_on(run_process(spec, cancellation));
        canceller.join().expect("canceller thread");

        assert!(matches!(result, Err(ToolError::UnknownOutcome { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn finishing_a_process_does_not_cancel_the_run_that_owns_the_token() {
        let cancellation = CancellationToken::new();
        let output = block_on(run_process(
            ProcessSpec::new("true", "."),
            cancellation.clone(),
        ))
        .expect("run");
        assert_eq!(output.status_code, Some(0));
        assert!(
            !cancellation.is_cancelled(),
            "the token is shared with the whole run and must survive one process"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_future_ends_the_process_group_without_cancelling_the_run() {
        let cancellation = CancellationToken::new();
        let marker = std::env::temp_dir().join(format!(
            "nyan-drop-{}-{}",
            std::process::id(),
            nyan_coding_agent_core::now_ms()
        ));
        let script = format!("sleep 1; touch {}", marker.display());
        let mut future = run_process(
            ProcessSpec::new("sh", ".").arg("-c").arg(script),
            cancellation.clone(),
        );
        // Poll once so the process starts, then abandon it.
        let mut context = Context::from_waker(Waker::noop());
        assert!(Pin::new(&mut future).poll(&mut context).is_pending());
        drop(future);
        thread::sleep(Duration::from_millis(1500));
        assert!(
            !marker.exists(),
            "the abandoned command must not keep running"
        );
        assert!(!cancellation.is_cancelled());
    }

    #[cfg(unix)]
    fn shell(script: &str) -> ProcessSpec {
        ProcessSpec::new("sh", ".").arg("-c").arg(script)
    }

    #[cfg(unix)]
    #[test]
    fn a_timeout_kills_the_group_and_keeps_the_output_so_far() {
        let started = std::time::Instant::now();
        let output = block_on(run_process(
            shell("echo before; sleep 30 & sleep 30").with_timeout(Duration::from_millis(300)),
            CancellationToken::new(),
        ))
        .expect("timed out run");
        assert!(output.timed_out);
        assert_eq!(output.status_code, None);
        assert_eq!(output.stdout, "before\n");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn a_background_grandchild_does_not_hold_the_result_hostage() {
        let started = std::time::Instant::now();
        let output = block_on(run_process(
            shell("sleep 30 & echo done"),
            CancellationToken::new(),
        ))
        .expect("run");
        assert_eq!(output.stdout, "done\n");
        assert_eq!(output.status_code, Some(0));
        assert!(!output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn streamed_output_arrives_in_chunks_and_is_not_captured() {
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink_seen = Arc::clone(&seen);
        let sink: super::OutputSink = Arc::new(move |chunk| {
            sink_seen
                .lock()
                .expect("sink lock")
                .extend_from_slice(chunk);
        });
        let output = block_on(run_process(
            shell("printf one; printf two >&2").with_output_sink(sink),
            CancellationToken::new(),
        ))
        .expect("run");
        assert_eq!((output.stdout.as_str(), output.stderr.as_str()), ("", ""));
        let mut bytes = seen.lock().expect("seen lock").clone();
        bytes.sort_unstable();
        let mut expected = b"onetwo".to_vec();
        expected.sort_unstable();
        assert_eq!(bytes, expected);
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let parker = Arc::new(ThreadParker {
            thread: thread::current(),
            ready: Mutex::new(false),
        });
        let waker = Waker::from(Arc::clone(&parker));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match Pin::new(&mut future).poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => {
                    let mut ready = parker.ready.lock().expect("ready lock");
                    while !*ready {
                        drop(ready);
                        thread::park();
                        ready = parker.ready.lock().expect("ready lock");
                    }
                    *ready = false;
                }
            }
        }
    }

    struct ThreadParker {
        thread: thread::Thread,
        ready: Mutex<bool>,
    }

    impl Wake for ThreadParker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            *self.ready.lock().expect("ready lock") = true;
            self.thread.unpark();
        }
    }
}
