//! The conversation id travels in the header a provider is told to use, and in
//! no other case.

use std::time::Duration;

use nyan_coding_agent_core::CancellationToken;
use nyan_coding_agent_provider::{ModelProvider, ProviderRequest};
use nyan_coding_agent_provider_http::{
    AnthropicMessagesProvider, GoogleGenerativeAiProvider, OpenAiChatProvider,
    OpenAiResponsesProvider,
};
use tokio::runtime::Runtime;

mod common;
use common::{drain, request, serve};

type Build = fn(&Runtime, &str, bool) -> Box<dyn ModelProvider>;

fn chat(runtime: &Runtime, base: &str, with_header: bool) -> Box<dyn ModelProvider> {
    let provider =
        OpenAiChatProvider::new("p", base, Some("k".to_owned()), runtime.handle().clone())
            .expect("provider")
            .with_idle_timeout(Duration::from_secs(5));
    Box::new(if with_header {
        provider.with_session_header("x-opencode-session")
    } else {
        provider
    })
}

fn responses(runtime: &Runtime, base: &str, with_header: bool) -> Box<dyn ModelProvider> {
    let provider =
        OpenAiResponsesProvider::new("p", base, Some("k".to_owned()), runtime.handle().clone())
            .expect("provider")
            .with_idle_timeout(Duration::from_secs(5));
    Box::new(if with_header {
        provider.with_session_header("x-opencode-session")
    } else {
        provider
    })
}

fn anthropic(runtime: &Runtime, base: &str, with_header: bool) -> Box<dyn ModelProvider> {
    let provider =
        AnthropicMessagesProvider::new("p", base, Some("k".to_owned()), runtime.handle().clone())
            .expect("provider")
            .with_idle_timeout(Duration::from_secs(5));
    Box::new(if with_header {
        provider.with_session_header("x-opencode-session")
    } else {
        provider
    })
}

fn google(runtime: &Runtime, base: &str, with_header: bool) -> Box<dyn ModelProvider> {
    let provider =
        GoogleGenerativeAiProvider::new("p", base, Some("k".to_owned()), runtime.handle().clone())
            .expect("provider")
            .with_idle_timeout(Duration::from_secs(5));
    Box::new(if with_header {
        provider.with_session_header("x-opencode-session")
    } else {
        provider
    })
}

fn sent(build: Build, with_header: bool, session_id: Option<&str>) -> String {
    let runtime = Runtime::new().expect("runtime");
    // Every protocol accepts a reply that ends at once; the outcome is not under test.
    let server = serve(
        &runtime,
        "200 OK",
        vec![(0, b"data: [DONE]\n\n".to_vec())],
        false,
    );
    let provider = build(&runtime, &format!("http://{}", server.address), with_header);
    let req = ProviderRequest {
        session_id: session_id.map(str::to_owned),
        ..request(CancellationToken::new())
    };
    if let Ok(stream) = runtime.block_on(provider.stream(req)) {
        let _ = drain(&runtime, stream);
    }
    server.captured.lock().expect("lock").to_ascii_lowercase()
}

#[test]
fn the_id_is_sent_in_the_named_header_by_every_protocol_that_can_be_asked_to() {
    for (name, build) in [
        ("chat", chat as Build),
        ("responses", responses),
        ("anthropic", anthropic),
        ("google", google),
    ] {
        let with = sent(build, true, Some("sess-42"));
        assert!(
            with.contains("\r\nx-opencode-session: sess-42\r\n"),
            "{name}: {with}"
        );
        let no_id = sent(build, true, None);
        assert!(
            !no_id.contains("x-opencode-session"),
            "{name}: no id, no header: {no_id}"
        );
        let no_header = sent(build, false, Some("sess-42"));
        assert!(
            !no_header.contains("sess-42"),
            "{name}: the id must not leak into a provider that did not ask for it: {no_header}"
        );
    }
}
