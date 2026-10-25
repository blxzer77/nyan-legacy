//! Process signals that interrupt a run.
//!
//! The first signal cancels the run through its [`CancellationToken`]: the model
//! request stops, running tools lose their process groups, the run ends as
//! aborted, the session records it, and the process exits with the signal's
//! code. A second signal means the first did not finish quickly, so the process
//! exits at once. TS print mode exits 143 on `SIGTERM` and 129 on `SIGHUP` and
//! dies to `SIGINT` (130 as the shell sees it); the codes here are the same.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::task::Poll;
use std::time::Duration;

use nyan_coding_agent_core::CancellationToken;
use nyan_coding_agent_runtime::RetrySleep;

/// Exit code after `SIGHUP`.
pub const EXIT_HANGUP: i32 = 129;
/// Exit code after an interrupt (Ctrl+C).
pub const EXIT_INTERRUPT: i32 = 130;
/// Exit code after a termination request.
pub const EXIT_TERMINATE: i32 = 143;

/// Which signal ended the run, if one did.
#[derive(Clone, Debug, Default)]
pub struct Interrupted(Arc<AtomicI32>);

impl Interrupted {
    /// The exit code of the first signal received, or `None` when none arrived.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        match self.0.load(Ordering::Acquire) {
            0 => None,
            code => Some(code),
        }
    }
}

/// Source of interrupt codes; one value per received signal.
#[cfg(unix)]
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn register() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    async fn next(&mut self) -> i32 {
        std::future::poll_fn(|context| {
            if self.interrupt.poll_recv(context).is_ready() {
                return Poll::Ready(EXIT_INTERRUPT);
            }
            if self.terminate.poll_recv(context).is_ready() {
                return Poll::Ready(EXIT_TERMINATE);
            }
            if self.hangup.poll_recv(context).is_ready() {
                return Poll::Ready(EXIT_HANGUP);
            }
            Poll::Pending
        })
        .await
    }
}

#[cfg(windows)]
struct Signals {
    interrupt: tokio::signal::windows::CtrlC,
    stop: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
}

#[cfg(windows)]
impl Signals {
    fn register() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::windows::ctrl_c()?,
            stop: tokio::signal::windows::ctrl_break()?,
            close: tokio::signal::windows::ctrl_close()?,
        })
    }

    async fn next(&mut self) -> i32 {
        std::future::poll_fn(|context| {
            if self.interrupt.poll_recv(context).is_ready() {
                return Poll::Ready(EXIT_INTERRUPT);
            }
            if self.stop.poll_recv(context).is_ready() {
                return Poll::Ready(EXIT_INTERRUPT);
            }
            if self.close.poll_recv(context).is_ready() {
                return Poll::Ready(EXIT_TERMINATE);
            }
            Poll::Pending
        })
        .await
    }
}

/// Starts listening; must run inside the tokio runtime that will drive the run.
///
/// # Errors
///
/// Returns the operating system error when a signal handler cannot be installed.
pub fn install(cancellation: CancellationToken) -> std::io::Result<Interrupted> {
    let mut signals = Signals::register()?;
    let interrupted = Interrupted::default();
    let record = Arc::clone(&interrupted.0);
    tokio::spawn(async move {
        let first = signals.next().await;
        record.store(first, Ordering::Release);
        cancellation.cancel();
        let second = signals.next().await;
        std::process::exit(second);
    });
    Ok(interrupted)
}

/// A retry wait that ends early when the run is cancelled.
#[must_use]
pub fn cancellable_sleep() -> RetrySleep {
    RetrySleep::new(|delay, cancellation| {
        Box::pin(async move {
            let slice = Duration::from_millis(50);
            let mut left = delay;
            while !left.is_zero() && !cancellation.is_cancelled() {
                let step = left.min(slice);
                tokio::time::sleep(step).await;
                left -= step;
            }
        })
    })
}
