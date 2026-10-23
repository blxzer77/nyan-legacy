//! HTTP model-provider transport for the Rust Coding Agent.
//!
//! Tokio is confined to this crate: providers take a runtime handle, and the
//! futures they return are executor-neutral.

pub mod openai;
pub mod sse;

pub use openai::OpenAiChatProvider;
