//! The model catalog, built from the models.dev provider map.
//!
//! This mirrors the per-provider sections of `packages/ai/scripts/generate-models.ts`
//! for the providers listed in [`SOURCES`]: which models.dev key feeds which
//! provider, which wire API and base URL a model gets, and which models are
//! skipped. A model is kept only if models.dev says it can call tools
//! (`tool_call`). The generator drops `deprecated` models for only a few
//! providers (see [`drops_deprecated`]); for the rest it keeps them, and so
//! does this code.
//!
//! Not ported from the generator: the providers it fills from live listings
//! (`OpenRouter`, Vercel AI Gateway, NVIDIA NIM), the hand-written entries
//! (`DeepSeek`, Ant Ling), the `GitHub` Copilot and Baseten
//! sections, the Qwen Token Plan individual allow-list, the Kimi implied
//! subscription costs, prices for the `openai-codex` list, tiered pricing, thinking-level maps, and `compat` flags.

use serde_json::{Map, Value};

use crate::WireApi;
use crate::table::{CODEX_MODELS, PROVIDERS};

/// Prices in US dollars per million tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    /// Input.
    pub input: f64,
    /// Output.
    pub output: f64,
    /// Input served from the provider cache.
    pub cache_read: f64,
    /// Input written to the provider cache.
    pub cache_write: f64,
}

/// One model a provider offers.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogModel {
    /// Provider id.
    pub provider: String,
    /// Model id, as requests send it.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The wire API the model is called through.
    pub api: WireApi,
    /// Base URL; may hold `{NAME}` placeholders. Empty when the user supplies it (Azure).
    pub base_url: String,
    /// Whether the model reasons.
    pub reasoning: bool,
    /// Whether the model accepts image input.
    pub image_input: bool,
    /// Context window in tokens.
    pub context_window: u64,
    /// Output limit in tokens.
    pub max_tokens: u64,
    /// Prices.
    pub cost: Cost,
}

/// How a provider's models are derived from its models.dev section.
#[derive(Clone, Copy, Debug)]
enum Rule {
    /// The provider's row in [`crate::table`] gives the API and base URL.
    Table,
    /// Gemini models only.
    VertexGemini,
    /// The wire API follows the models.dev `provider.npm` field.
    Opencode(&'static str),
    /// Ids are `<upstream>/<model>`; the upstream picks the API.
    CloudflareGateway,
    /// `glm-` and `kimi-k3` use chat completions, the rest Anthropic Messages.
    Fireworks,
}

struct Source {
    provider: &'static str,
    dev_keys: &'static [&'static str],
    rule: Rule,
}

const fn table(provider: &'static str) -> Source {
    Source {
        provider,
        dev_keys: &[],
        rule: Rule::Table,
    }
}

const fn aliased(provider: &'static str, dev_keys: &'static [&'static str]) -> Source {
    Source {
        provider,
        dev_keys,
        rule: Rule::Table,
    }
}

/// Every provider the catalog can list, in the order it lists them.
const SOURCES: &[Source] = &[
    table("anthropic"),
    table("openai"),
    table("groq"),
    table("cerebras"),
    table("cloudflare-workers-ai"),
    Source {
        provider: "cloudflare-ai-gateway",
        dev_keys: &[],
        rule: Rule::CloudflareGateway,
    },
    table("xai"),
    table("huggingface"),
    Source {
        provider: "fireworks",
        dev_keys: &["fireworks-ai"],
        rule: Rule::Fireworks,
    },
    aliased("together", &["together", "togetherai", "together-ai"]),
    Source {
        provider: "opencode",
        dev_keys: &[],
        rule: Rule::Opencode("https://opencode.ai/zen"),
    },
    Source {
        provider: "opencode-go",
        dev_keys: &[],
        rule: Rule::Opencode("https://opencode.ai/zen/go"),
    },
    table("google"),
    Source {
        provider: "google-vertex",
        dev_keys: &[],
        rule: Rule::VertexGemini,
    },
    table("moonshotai"),
    table("moonshotai-cn"),
    table("xiaomi"),
    table("xiaomi-token-plan-cn"),
    table("xiaomi-token-plan-ams"),
    table("xiaomi-token-plan-sgp"),
    aliased("qwen-token-plan", &["alibaba-token-plan"]),
    aliased("qwen-token-plan-cn", &["alibaba-token-plan-cn"]),
    aliased("zai", &["zai-coding-plan"]),
    aliased("zai-coding-cn", &["zhipuai-coding-plan"]),
];

/// The providers the catalog can list.
#[must_use]
pub fn catalog_providers() -> Vec<&'static str> {
    SOURCES
        .iter()
        .map(|source| source.provider)
        .chain(["openai-codex"])
        .collect()
}

fn number(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

fn limit(model: &Value, name: &str) -> u64 {
    model
        .pointer(&format!("/limit/{name}"))
        .and_then(Value::as_u64)
        .filter(|tokens| *tokens > 0)
        .unwrap_or(4096)
}

fn cost_of(model: &Value) -> Cost {
    Cost {
        input: number(model.pointer("/cost/input")),
        output: number(model.pointer("/cost/output")),
        cache_read: number(model.pointer("/cost/cache_read")),
        cache_write: number(model.pointer("/cost/cache_write")),
    }
}

/// Whether the generator skips `status: "deprecated"` models for `provider`.
fn drops_deprecated(provider: &str) -> bool {
    matches!(
        provider,
        "together"
            | "opencode"
            | "opencode-go"
            | "xiaomi"
            | "xiaomi-token-plan-cn"
            | "xiaomi-token-plan-ams"
            | "xiaomi-token-plan-sgp"
    )
}

fn is_listed(provider: &str, model: &Value) -> bool {
    model.get("tool_call").and_then(Value::as_bool) == Some(true)
        && !(drops_deprecated(provider)
            && model.get("status").and_then(Value::as_str) == Some("deprecated"))
}

fn models_of<'a>(
    providers: &'a Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Map<String, Value>> {
    keys.iter()
        .find_map(|key| providers.get(*key)?.get("models")?.as_object())
}

/// The fields every rule shares.
fn base_model(
    provider: &str,
    id: &str,
    model: &Value,
    api: WireApi,
    base_url: &str,
) -> CatalogModel {
    CatalogModel {
        provider: provider.to_owned(),
        id: id.to_owned(),
        name: model
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(id)
            .to_owned(),
        api,
        base_url: base_url.to_owned(),
        reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
        image_input: model
            .pointer("/modalities/input")
            .and_then(Value::as_array)
            .is_some_and(|input| input.iter().any(|kind| kind == "image")),
        context_window: limit(model, "context"),
        max_tokens: limit(model, "output"),
        cost: cost_of(model),
    }
}

fn table_row(provider: &str) -> Option<(WireApi, &'static str)> {
    PROVIDERS
        .iter()
        .find(|def| def.id == provider)
        .map(|def| (def.api, def.base_url.unwrap_or_default()))
}

/// Builds the catalog from a models.dev provider map.
#[must_use]
pub fn build(providers: &Map<String, Value>) -> Vec<CatalogModel> {
    let mut catalog: Vec<CatalogModel> = Vec::new();
    for source in SOURCES {
        let keys: Vec<&str> = if source.dev_keys.is_empty() {
            vec![source.provider]
        } else {
            source.dev_keys.to_vec()
        };
        if let Some(models) = models_of(providers, &keys) {
            catalog.extend(models_for(source.provider, source.rule, models));
        }
    }
    catalog.extend(CODEX_MODELS.iter().map(|model| CatalogModel {
        provider: "openai-codex".to_owned(),
        id: model.id.to_owned(),
        name: model.name.to_owned(),
        api: WireApi::OpenAiCodexResponses,
        base_url: "https://chatgpt.com/backend-api".to_owned(),
        reasoning: model.reasoning,
        image_input: model.image_input,
        context_window: model.context_window,
        max_tokens: model.max_tokens,
        cost: Cost::default(),
    }));
    catalog
}

fn models_for(provider: &str, rule: Rule, models: &Map<String, Value>) -> Vec<CatalogModel> {
    let mut out = Vec::new();
    for (id, model) in models {
        if !is_listed(provider, model) {
            continue;
        }
        let built = match rule {
            Rule::Table => table_model(provider, id, model, models),
            Rule::VertexGemini => vertex_model(id, model, models),
            Rule::Opencode(base_path) => Some(opencode_model(provider, base_path, id, model)),
            Rule::CloudflareGateway => cloudflare_gateway_model(id, model),
            Rule::Fireworks => Some(fireworks_model(id, model)),
        };
        out.extend(built);
    }
    out
}

/// xAI models the generator leaves out of the built-in list.
const XAI_EXCLUDED: &[&str] = &[
    "grok-3",
    "grok-3-fast",
    "grok-4.20-0309-non-reasoning",
    "grok-4.20-0309-reasoning",
    "grok-build-0.1",
    "grok-code-fast-1",
];

fn table_model(
    provider: &str,
    id: &str,
    model: &Value,
    all: &Map<String, Value>,
) -> Option<CatalogModel> {
    let (api, base_url) = table_row(provider)?;
    match provider {
        "openai" if id == "gpt-5.6" => return None,
        "qwen-token-plan" | "qwen-token-plan-cn" if id == "qwen3.8-max-preview" => return None,
        "xai" if XAI_EXCLUDED.contains(&id) => return None,
        _ => {}
    }
    // Google's "-latest" aliases take their limits and prices from the model they point at.
    let source = if provider == "google" {
        match id {
            "gemini-flash-latest" => all.get("gemini-3.5-flash").unwrap_or(model),
            "gemini-flash-lite-latest" => all.get("gemini-3.1-flash-lite").unwrap_or(model),
            _ => model,
        }
    } else {
        model
    };
    let mut built = base_model(provider, id, source, api, base_url);
    built.name = base_model(provider, id, model, api, base_url).name;
    Some(built)
}

fn vertex_model(id: &str, model: &Value, all: &Map<String, Value>) -> Option<CatalogModel> {
    if !id.starts_with("gemini-") || id == "gemini-3.1-flash-lite-preview" {
        return None;
    }
    let source = match id {
        "gemini-flash-latest" => all.get("gemini-3.5-flash").unwrap_or(model),
        "gemini-flash-lite-latest" => all.get("gemini-3.1-flash-lite").unwrap_or(model),
        _ => model,
    };
    let (api, base_url) = table_row("google-vertex")?;
    let mut built = base_model("google-vertex", id, source, api, base_url);
    built.name = base_model("google-vertex", id, model, api, base_url).name;
    built.cost.cache_write = 0.0;
    if id == "gemini-2.5-flash" {
        // models.dev's Vertex cache price for this model does not match the published one.
        built.cost.cache_read = 0.03;
    }
    Some(built)
}

fn opencode_model(provider: &str, base_path: &str, id: &str, model: &Value) -> CatalogModel {
    let npm = model.pointer("/provider/npm").and_then(Value::as_str);
    let (mut api, mut base_url) = match npm {
        Some("@ai-sdk/openai") => (WireApi::OpenAiResponses, format!("{base_path}/v1")),
        Some("@ai-sdk/anthropic") => (WireApi::AnthropicMessages, base_path.to_owned()),
        Some("@ai-sdk/google") => (WireApi::GoogleGenerativeAi, format!("{base_path}/v1")),
        _ => (WireApi::OpenAiCompletions, format!("{base_path}/v1")),
    };
    // models.dev lists these as Anthropic or generic, but the endpoint serves them
    // through the chat-completions path.
    if provider == "opencode-go" && matches!(id, "minimax-m2.7" | "qwen3.5-plus" | "qwen3.6-plus") {
        api = WireApi::OpenAiCompletions;
        base_url = format!("{base_path}/v1");
    }
    base_model(provider, id, model, api, &base_url)
}

fn cloudflare_gateway_model(prefixed: &str, model: &Value) -> Option<CatalogModel> {
    const GATEWAY: &str =
        "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}";
    let (upstream, native) = prefixed.split_once('/')?;
    let (api, base_url, id) = match upstream {
        "openai" => (
            WireApi::OpenAiResponses,
            format!("{GATEWAY}/openai"),
            native.to_owned(),
        ),
        // Anthropic accepts dashed ids; models.dev lists dotted versions.
        "anthropic" => (
            WireApi::AnthropicMessages,
            format!("{GATEWAY}/anthropic"),
            native.replace('.', "-"),
        ),
        "workers-ai" => (
            WireApi::OpenAiCompletions,
            format!("{GATEWAY}/compat"),
            prefixed.to_owned(),
        ),
        _ => return None,
    };
    Some(base_model(
        "cloudflare-ai-gateway",
        &id,
        model,
        api,
        &base_url,
    ))
}

fn fireworks_model(id: &str, model: &Value) -> CatalogModel {
    if id.contains("glm-") || id.contains("kimi-k3") {
        base_model(
            "fireworks",
            id,
            model,
            WireApi::OpenAiCompletions,
            "https://api.fireworks.ai/inference/v1",
        )
    } else {
        base_model(
            "fireworks",
            id,
            model,
            WireApi::AnthropicMessages,
            "https://api.fireworks.ai/inference",
        )
    }
}

/// A built catalog with lookups.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    models: Vec<CatalogModel>,
}

impl Catalog {
    /// Builds the catalog from a models.dev provider map.
    #[must_use]
    pub fn from_models_dev(providers: &Map<String, Value>) -> Self {
        Self {
            models: build(providers),
        }
    }

    /// Every model.
    #[must_use]
    pub fn models(&self) -> &[CatalogModel] {
        &self.models
    }

    /// One model.
    #[must_use]
    pub fn find(&self, provider: &str, id: &str) -> Option<&CatalogModel> {
        self.models
            .iter()
            .find(|model| model.provider == provider && model.id == id)
    }

    /// The models of one provider.
    pub fn for_provider<'a>(&'a self, provider: &'a str) -> impl Iterator<Item = &'a CatalogModel> {
        self.models
            .iter()
            .filter(move |model| model.provider == provider)
    }
}

#[cfg(test)]
mod tests;
