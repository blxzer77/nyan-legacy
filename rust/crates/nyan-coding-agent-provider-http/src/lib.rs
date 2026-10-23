//! HTTP model-provider transport for the Rust Coding Agent.
//!
//! Tokio is confined to this crate: providers take a runtime handle, and the
//! futures they return are executor-neutral.

pub mod anthropic;
pub mod cpa;
pub mod google;
pub mod mistral;
pub mod openai;
pub mod responses;
pub mod sse;
pub(crate) mod transport;

pub use anthropic::AnthropicMessagesProvider;
pub use google::{GoogleGenerativeAiProvider, GoogleVertexProvider};
pub use mistral::MistralConversationsProvider;
pub use openai::OpenAiChatProvider;
pub use responses::OpenAiResponsesProvider;
