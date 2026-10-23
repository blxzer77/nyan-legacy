//! HTTP model-provider transport for the Rust Coding Agent.
//!
//! Tokio is confined to this crate: providers take a runtime handle, and the
//! futures they return are executor-neutral.

pub mod cpa;
pub mod openai;
pub mod responses;
pub mod sse;
pub(crate) mod transport;

pub use openai::OpenAiChatProvider;
pub use responses::OpenAiResponsesProvider;
