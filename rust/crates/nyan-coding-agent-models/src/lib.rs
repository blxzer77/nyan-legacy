//! Provider definitions and the model catalog for the Rust Coding Agent.
//!
//! - [`table`]: which wire API, base URL, and key variable each provider uses,
//!   generated from the TypeScript provider definitions.
//! - [`catalog`]: the model list, built from the public models.dev data.
//! - [`cache`]: the on-disk copy of that data, in the file the TypeScript CLI
//!   also writes, so the two never fetch the same bytes twice.
//!
//! Nothing here touches the network; the fetch lives in the HTTP crate.

pub mod cache;
pub mod catalog;
pub mod table;

/// The wire APIs this build speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireApi {
    /// `OpenAI` chat completions and its many compatibles.
    OpenAiCompletions,
    /// `OpenAI` Responses.
    OpenAiResponses,
    /// Anthropic Messages.
    AnthropicMessages,
    /// Google Generative AI.
    GoogleGenerativeAi,
    /// Google Vertex AI.
    GoogleVertex,
}

impl WireApi {
    /// The name the TypeScript catalog uses for this API.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "openai-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
            Self::GoogleGenerativeAi => "google-generative-ai",
            Self::GoogleVertex => "google-vertex",
        }
    }
}
