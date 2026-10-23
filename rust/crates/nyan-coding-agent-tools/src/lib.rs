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
use std::time::Duration;

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

/// A managed subprocess specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    /// Executable name or path.
    pub program: String,
    /// Arguments in operating-system order.
    pub args: Vec<String>,
    /// Working directory for the child.
    pub cwd: PathBuf,
}

impl ProcessSpec {
    /// Creates a process specification.
    #[must_use]
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
        }
    }

    /// Appends one argument and returns the updated specification.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.args.push(argument.into());
        self
    }
}

/// Captured result of a managed process group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    /// Exit code, or `None` when the process terminated by signal.
    pub status_code: Option<i32>,
    /// Complete captured stdout.
    pub stdout: String,
    /// Complete captured stderr.
    pub stderr: String,
}

/// Runs one subprocess group and owns it until exit or cancellation.
///
/// The returned future does not choose an async executor. The process group is
/// polled by a dedicated standard-library thread, and cancellation kills the
/// group before the future resolves. Dropping the future also requests cancellation.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when spawning or capturing output fails,
/// [`ToolError::Cancelled`] when cancellation happens before spawn, and
/// [`ToolError::UnknownOutcome`] when a running process group is terminated or
/// its completion cannot be proven.
pub fn run_process(
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> BoxFuture<Result<ProcessOutput, ToolError>> {
    let (sender, receiver) = mpsc::channel();
    let state = Arc::new(ProcessFutureState {
        done: AtomicBool::new(false),
        waker: Mutex::new(None),
        cancellation: cancellation.clone(),
    });
    let thread_state = Arc::clone(&state);
    thread::spawn(move || {
        let result = execute_process(spec, cancellation);
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
    cancellation: CancellationToken,
}

struct ProcessFuture {
    receiver: Receiver<Result<ProcessOutput, ToolError>>,
    state: Arc<ProcessFutureState>,
}

impl Drop for ProcessFuture {
    fn drop(&mut self) {
        self.state.cancellation.cancel();
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

// The worker owns both values so the caller can return immediately while the child is alive.
#[allow(clippy::needless_pass_by_value)]
fn execute_process(
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> Result<ProcessOutput, ToolError> {
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
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
    let stdout_reader = thread::spawn(move || read_pipe(&mut stdout));
    let stderr_reader = thread::spawn(move || read_pipe(&mut stderr));

    let status = loop {
        if cancellation.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            discard_pipe(stdout_reader);
            discard_pipe(stderr_reader);
            return Err(ToolError::UnknownOutcome {
                operation: spec.program.clone(),
                message: "process group was terminated before completion".to_owned(),
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
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
    let stdout = join_pipe(stdout_reader)?;
    let stderr = join_pipe(stderr_reader)?;
    Ok(ProcessOutput {
        status_code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn read_pipe(reader: &mut impl Read) -> Result<Vec<u8>, ToolError> {
    let mut output = Vec::new();
    reader
        .read_to_end(&mut output)
        .map_err(|error| ToolError::Execution {
            message: format!("failed to read child output: {error}"),
        })?;
    Ok(output)
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
