//! Non-interactive runs: print mode and the JSONL event stream.
//!
//! Behavior follows `modes/durable/print.ts` and `modes/durable/json.ts`:
//! prompts run in order on one conversation, only the last answer is printed in
//! print mode, and an answer that ended in `error`, `aborted`, or `length`
//! makes the process fail.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nyan_coding_agent_core::{Message, MessageRole, StopReason, now_ms};
use nyan_coding_agent_protocol::{event_to_jsonl, session_header_jsonl};
use nyan_coding_agent_provider::{ProviderModel, ProviderRegistry};
use nyan_coding_agent_runtime::{AgentContext, AgentEvent, AgentRuntime, EventSink, RuntimeConfig};
use nyan_coding_agent_tools::ToolRegistry;

/// Which headless output to produce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadlessMode {
    /// Final answer as plain text.
    Print,
    /// Session header and every event as JSONL.
    Json,
}

/// Inputs for one headless invocation.
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    /// Output mode.
    pub mode: HeadlessMode,
    /// Prompts to run in order.
    pub messages: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Selected provider and model.
    pub model: ProviderModel,
    /// The system prompt, placed first in every request.
    pub system_prompt: Option<String>,
}

/// A byte sink shared between the caller and the event sink.
pub type SharedWriter = Arc<Mutex<dyn Write + Send>>;

struct JsonlSink {
    out: SharedWriter,
}

impl EventSink for JsonlSink {
    fn emit(&self, event: AgentEvent) {
        let line = event_to_jsonl(&event);
        if let Ok(mut out) = self.out.lock() {
            // A reader that went away (a closed pipe) must not abort the run.
            let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
        }
    }
}

struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: AgentEvent) {}
}

/// Generates a random version-4 UUID string without an external crate.
#[must_use]
pub fn new_session_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut bytes = [0_u8; 16];
    for half in bytes.chunks_mut(8) {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(u128::from(now_ms()));
        hasher.write_u32(std::process::id());
        half.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Runs the prompts and returns the process exit code.
///
/// `out` receives stdout and `err` receives stderr. In JSON mode stdout carries
/// only JSONL records; every diagnostic goes to `err`.
pub async fn run_headless(
    options: HeadlessOptions,
    providers: &dyn ProviderRegistry,
    tools: &dyn ToolRegistry,
    out: SharedWriter,
    err: SharedWriter,
) -> i32 {
    let write_err = |text: &str| {
        if let Ok(mut err) = err.lock() {
            let _ = err.write_all(text.as_bytes()).and_then(|()| err.flush());
        }
    };
    let sink: Arc<dyn EventSink> = match options.mode {
        HeadlessMode::Json => {
            let header = session_header_jsonl(
                &new_session_id(),
                now_ms(),
                &options.cwd.display().to_string(),
            );
            if let Ok(mut out) = out.lock() {
                let _ = out.write_all(header.as_bytes()).and_then(|()| out.flush());
            }
            Arc::new(JsonlSink {
                out: Arc::clone(&out),
            })
        }
        HeadlessMode::Print => Arc::new(NullSink),
    };

    let mut runtime = AgentRuntime::new(
        providers,
        tools,
        RuntimeConfig::new(options.model.clone(), options.cwd.clone()),
    );
    if let Some(prompt) = &options.system_prompt {
        runtime.set_context(AgentContext {
            messages: vec![Message::text(MessageRole::System, prompt.clone())],
        });
    }
    let mut last_answer: Option<Message> = None;
    for text in &options.messages {
        let result = runtime
            .run(Message::text(MessageRole::User, text.clone()), sink.clone())
            .await;
        last_answer = result
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .cloned();
    }

    let Some(answer) = last_answer else {
        return 0;
    };
    match answer.stop_reason {
        Some(reason @ (StopReason::Error | StopReason::Aborted)) => {
            if options.mode == HeadlessMode::Print {
                let fallback = if reason == StopReason::Error {
                    "Request error"
                } else {
                    "Request aborted"
                };
                write_err(&format!(
                    "{}\n",
                    answer.error_message.as_deref().unwrap_or(fallback)
                ));
            }
            1
        }
        Some(StopReason::Length) => {
            if options.mode == HeadlessMode::Print {
                if !answer.text.is_empty()
                    && let Ok(mut out) = out.lock()
                {
                    let _ = writeln!(out, "{}", answer.text);
                }
                write_err("Response was truncated: the model reached its output token limit.\n");
            }
            1
        }
        _ => {
            if options.mode == HeadlessMode::Print
                && !answer.text.is_empty()
                && let Ok(mut out) = out.lock()
            {
                let _ = out.write_all(format!("{}\n", answer.text).as_bytes());
                let _ = out.flush();
            }
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;

    use nyan_coding_agent_core::{BoxFuture, CoreError};
    use nyan_coding_agent_provider::{
        ModelProvider, ProviderError, ProviderModel, ProviderRegistry, ProviderRequest,
        ProviderStopReason, ProviderStream, ProviderStreamEvent,
    };
    use nyan_coding_agent_tools::{Tool, ToolDefinition, ToolRegistry};

    use super::{HeadlessMode, HeadlessOptions, SharedWriter, new_session_id, run_headless};

    struct Scripted {
        responses: Mutex<VecDeque<Vec<Result<ProviderStreamEvent, ProviderError>>>>,
    }

    struct ScriptedStream {
        events: VecDeque<Result<ProviderStreamEvent, ProviderError>>,
    }

    impl ProviderStream for ScriptedStream {
        fn next(&mut self) -> BoxFuture<Result<Option<ProviderStreamEvent>, ProviderError>> {
            let next = self.events.pop_front();
            Box::pin(async move { next.transpose() })
        }
    }

    impl ModelProvider for Scripted {
        fn id(&self) -> &'static str {
            "scripted"
        }

        fn stream(
            &self,
            _request: ProviderRequest,
        ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
            let events = self
                .responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .unwrap_or_default();
            Box::pin(async move {
                Ok(Box::new(ScriptedStream {
                    events: events.into(),
                }) as Box<dyn ProviderStream>)
            })
        }
    }

    impl ProviderRegistry for Scripted {
        fn provider(&self, id: &str) -> Result<&dyn ModelProvider, CoreError> {
            if id == "scripted" {
                Ok(self)
            } else {
                Err(CoreError::Unsupported {
                    operation: format!("provider {id}"),
                })
            }
        }
    }

    struct NoTools;

    impl ToolRegistry for NoTools {
        fn tool(&self, _name: &str) -> Option<&dyn Tool> {
            None
        }

        fn definitions(&self) -> Vec<ToolDefinition> {
            Vec::new()
        }
    }

    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("capture lock")
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture() -> (SharedWriter, Arc<Mutex<Vec<u8>>>) {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        (Arc::new(Mutex::new(Capture(Arc::clone(&bytes)))), bytes)
    }

    fn text(bytes: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(bytes.lock().expect("capture lock").clone()).expect("utf8 output")
    }

    fn answer(
        text: &str,
        reason: ProviderStopReason,
    ) -> Vec<Result<ProviderStreamEvent, ProviderError>> {
        vec![
            Ok(ProviderStreamEvent::TextDelta {
                text: text.to_owned(),
            }),
            Ok(ProviderStreamEvent::Finished { reason }),
        ]
    }

    fn run(
        mode: HeadlessMode,
        messages: &[&str],
        responses: Vec<Vec<Result<ProviderStreamEvent, ProviderError>>>,
    ) -> (i32, String, String) {
        let provider = Scripted {
            responses: Mutex::new(responses.into()),
        };
        let (out, out_bytes) = capture();
        let (err, err_bytes) = capture();
        let code = block_on(run_headless(
            HeadlessOptions {
                mode,
                messages: messages
                    .iter()
                    .map(|message| (*message).to_owned())
                    .collect(),
                cwd: "/tmp/project".into(),
                model: ProviderModel {
                    provider: "scripted".to_owned(),
                    model: "m".to_owned(),
                },
                system_prompt: None,
            },
            &provider,
            &NoTools,
            out,
            err,
        ));
        (code, text(&out_bytes), text(&err_bytes))
    }

    #[test]
    fn print_mode_prints_only_the_last_answer() {
        let (code, out, err) = run(
            HeadlessMode::Print,
            &["one", "two"],
            vec![
                answer("first", ProviderStopReason::Stop),
                answer("second", ProviderStopReason::Stop),
            ],
        );
        assert_eq!((code, out.as_str(), err.as_str()), (0, "second\n", ""));
    }

    #[test]
    fn print_mode_reports_a_provider_error_on_stderr_and_fails() {
        let (code, out, err) = run(
            HeadlessMode::Print,
            &["go"],
            vec![vec![Ok(ProviderStreamEvent::Finished {
                reason: ProviderStopReason::Error {
                    message: "model overloaded".to_owned(),
                },
            })]],
        );
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (1, "", "model overloaded\n")
        );
    }

    #[test]
    fn a_truncated_answer_is_printed_and_still_fails() {
        let (code, out, err) = run(
            HeadlessMode::Print,
            &["go"],
            vec![answer("partial", ProviderStopReason::Length)],
        );
        assert_eq!(code, 1);
        assert_eq!(out, "partial\n");
        assert_eq!(
            err,
            "Response was truncated: the model reached its output token limit.\n"
        );
    }

    #[test]
    fn json_mode_fails_on_a_truncated_answer_without_extra_stdout() {
        let (code, out, err) = run(
            HeadlessMode::Json,
            &["go"],
            vec![answer("partial", ProviderStopReason::Length)],
        );
        assert_eq!(code, 1);
        assert_eq!(err, "");
        assert!(out.contains("\"stopReason\":\"length\""), "{out}");
    }

    #[test]
    fn json_mode_writes_the_session_header_then_events_and_nothing_else_on_stdout() {
        let (code, out, err) = run(
            HeadlessMode::Json,
            &["hello"],
            vec![answer("hi", ProviderStopReason::Stop)],
        );
        assert_eq!(code, 0);
        assert_eq!(err, "");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("{\"type\":\"session\",\"version\":3,\"id\":\""));
        assert!(lines[0].ends_with("\"cwd\":\"/tmp/project\"}"));
        let types: Vec<String> = lines
            .iter()
            .map(|line| {
                let value: serde_json::Value =
                    serde_json::from_str(line).expect("each line is JSON");
                value["type"].as_str().expect("type field").to_owned()
            })
            .collect();
        assert_eq!(
            types,
            [
                "session",
                "agent_start",
                "message_start",
                "message_end",
                "turn_start",
                "message_start",
                "message_update",
                "message_update",
                "message_update",
                "message_end",
                "turn_end",
                "agent_end",
                "agent_settled",
            ]
        );
        assert!(out.ends_with('\n'));
        assert_eq!(out.matches('\n').count(), lines.len());
    }

    #[test]
    fn json_mode_fails_when_the_answer_errored_but_keeps_stdout_pure() {
        let (code, out, err) = run(
            HeadlessMode::Json,
            &["go"],
            vec![vec![Ok(ProviderStreamEvent::Finished {
                reason: ProviderStopReason::Error {
                    message: "boom".to_owned(),
                },
            })]],
        );
        assert_eq!(code, 1);
        assert_eq!(
            err, "",
            "JSON mode keeps diagnostics inside the event stream"
        );
        for line in out.lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("stdout is only JSONL");
        }
    }

    #[test]
    fn no_prompts_is_a_successful_no_op() {
        let (code, out, _) = run(HeadlessMode::Print, &[], vec![]);
        assert_eq!((code, out.as_str()), (0, ""));
    }

    #[test]
    fn session_ids_are_distinct_version_4_uuids() {
        let first = new_session_id();
        let second = new_session_id();
        assert_ne!(first, second);
        for id in [&first, &second] {
            let parts: Vec<&str> = id.split('-').collect();
            assert_eq!(
                parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
                [8, 4, 4, 4, 12]
            );
            assert!(parts[2].starts_with('4'));
            assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
        }
    }

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
}
