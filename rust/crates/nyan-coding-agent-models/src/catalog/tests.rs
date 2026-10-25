#![allow(clippy::unreadable_literal)] // fixture numbers mirror the models.dev data

use serde_json::{Map, Value, json};

use super::{Catalog, CatalogModel, Cost, catalog_providers};
use crate::WireApi;

fn dev(value: &Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

fn model(tool_call: bool) -> Value {
    json!({
        "name": "Some Model", "tool_call": tool_call, "reasoning": true,
        "modalities": {"input": ["text", "image"], "output": ["text"]},
        "limit": {"context": 200000, "output": 32000},
        "cost": {"input": 3.0, "output": 15.0, "cache_read": 0.3, "cache_write": 3.75},
    })
}

fn only(catalog: &Catalog, provider: &str) -> Vec<CatalogModel> {
    catalog.for_provider(provider).cloned().collect()
}

#[test]
fn a_table_provider_maps_limits_prices_and_modalities_and_skips_what_cannot_call_tools() {
    let catalog = Catalog::from_models_dev(&dev(&json!({"anthropic": {"models": {
        "claude-a": model(true),
        "no-tools": model(false),
        "bare": {"tool_call": true},
    }}})));
    let models = only(&catalog, "anthropic");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["bare", "claude-a"],
        "sorted by id; the model without tool calls is dropped"
    );
    assert_eq!(
        models[1],
        CatalogModel {
            provider: "anthropic".to_owned(),
            id: "claude-a".to_owned(),
            name: "Some Model".to_owned(),
            api: WireApi::AnthropicMessages,
            base_url: "https://api.anthropic.com".to_owned(),
            reasoning: true,
            image_input: true,
            context_window: 200_000,
            max_tokens: 32_000,
            cost: Cost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75
            },
        }
    );
    let bare = &models[0];
    assert_eq!(
        (bare.name.as_str(), bare.context_window, bare.max_tokens),
        ("bare", 4096, 4096)
    );
    assert!(!bare.reasoning && !bare.image_input);
    assert_eq!(bare.cost, Cost::default());
}

#[test]
fn aliased_sources_feed_their_providers() {
    let catalog = Catalog::from_models_dev(&dev(&json!({
        "togetherai": {"models": {"t": model(true)}},
        "alibaba-token-plan": {"models": {"q": model(true), "qwen3.8-max-preview": model(true)}},
        "alibaba-token-plan-cn": {"models": {"qc": model(true)}},
        "zai-coding-plan": {"models": {"z": model(true)}},
        "zhipuai-coding-plan": {"models": {"zc": model(true)}},
    })));
    assert_eq!(only(&catalog, "together")[0].id, "t");
    assert_eq!(
        only(&catalog, "qwen-token-plan").len(),
        1,
        "the preview model is excluded"
    );
    assert_eq!(only(&catalog, "qwen-token-plan-cn")[0].id, "qc");
    assert_eq!(only(&catalog, "zai")[0].id, "z");
    assert_eq!(only(&catalog, "zai-coding-cn")[0].id, "zc");
    assert_eq!(
        only(&catalog, "together")[0].api,
        WireApi::OpenAiCompletions
    );
}

#[test]
fn openai_is_mirrored_to_azure_with_the_azure_context_windows() {
    let catalog = Catalog::from_models_dev(&dev(&json!({"openai": {"models": {
        "gpt-5.5": model(true), "gpt-5.6": model(true), "gpt-x": model(true),
    }}})));
    let openai: Vec<_> = only(&catalog, "openai")
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(
        openai,
        ["gpt-5.5", "gpt-x"],
        "gpt-5.6 is unsupported on the API"
    );
    assert!(openai.iter().all(|_| true));
    let azure = only(&catalog, "azure");
    assert_eq!(azure.len(), 2);
    assert!(
        azure
            .iter()
            .all(|m| m.api == WireApi::AzureOpenAiResponses && m.base_url.is_empty())
    );
    assert_eq!(azure[0].context_window, 1_050_000);
    assert_eq!(azure[1].context_window, 200_000);
    assert_eq!(
        only(&catalog, "openai")[0].context_window,
        200_000,
        "OpenAI itself is unchanged"
    );
}

#[test]
fn bedrock_skips_known_unusable_models_and_routes_eu_ids_to_frankfurt() {
    let catalog = Catalog::from_models_dev(&dev(&json!({"amazon-bedrock": {"models": {
        "anthropic.claude-opus-5": model(true),
        "ai21.jamba-1": model(true),
        "mistral.mistral-7b-instruct-v0:2": model(true),
        "eu.anthropic.claude-x": model(true),
        "anthropic.claude-y": model(true),
    }}})));
    let models = only(&catalog, "amazon-bedrock");
    let urls: Vec<(&str, &str)> = models
        .iter()
        .map(|m| (m.id.as_str(), m.base_url.as_str()))
        .collect();
    assert_eq!(
        urls,
        [
            (
                "anthropic.claude-y",
                "https://bedrock-runtime.us-east-1.amazonaws.com"
            ),
            (
                "eu.anthropic.claude-x",
                "https://bedrock-runtime.eu-central-1.amazonaws.com"
            ),
        ]
    );
    assert!(models.iter().all(|m| m.api == WireApi::BedrockConverse));
}

#[test]
fn vertex_lists_only_gemini_models_and_resolves_latest_aliases() {
    let mut flash = model(true);
    flash["limit"] = json!({"context": 1_000_000, "output": 65_000});
    let mut alias = model(true);
    alias["name"] = json!("Gemini Flash Latest");
    let catalog = Catalog::from_models_dev(&dev(&json!({
        "google-vertex": {"models": {
            "claude-on-vertex": model(true),
            "gemini-3.1-flash-lite-preview": model(true),
            "gemini-3.5-flash": flash.clone(),
            "gemini-flash-latest": alias.clone(),
            "gemini-2.5-flash": model(true),
        }},
        "google": {"models": {"gemini-3.5-flash": flash, "gemini-flash-latest": alias}},
    })));
    let vertex = only(&catalog, "google-vertex");
    let ids: Vec<&str> = vertex.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "gemini-2.5-flash",
            "gemini-3.5-flash",
            "gemini-flash-latest"
        ]
    );
    let latest = vertex
        .iter()
        .find(|m| m.id == "gemini-flash-latest")
        .expect("alias");
    assert_eq!(
        (latest.context_window, latest.name.as_str()),
        (1_000_000, "Gemini Flash Latest")
    );
    assert_eq!(vertex[0].cost.cache_read, 0.03, "the corrected cache price");
    assert_eq!(vertex[0].cost.cache_write, 0.0);
    assert!(vertex.iter().all(|m| m.api == WireApi::GoogleVertex));
    let google_latest = catalog
        .find("google", "gemini-flash-latest")
        .expect("google alias");
    assert_eq!(google_latest.context_window, 1_000_000);
    assert_eq!(
        google_latest.base_url,
        "https://generativelanguage.googleapis.com/v1beta"
    );
}

#[test]
fn opencode_picks_the_api_from_the_npm_package_with_endpoint_exceptions() {
    let with_npm = |npm: Option<&str>| {
        let mut value = model(true);
        if let Some(npm) = npm {
            value["provider"] = json!({"npm": npm});
        }
        value
    };
    let catalog = Catalog::from_models_dev(&dev(&json!({
        "opencode": {"models": {
            "r": with_npm(Some("@ai-sdk/openai")),
            "a": with_npm(Some("@ai-sdk/anthropic")),
            "g": with_npm(Some("@ai-sdk/google")),
            "c": with_npm(None),
        }},
        "opencode-go": {"models": {
            "minimax-m2.7": with_npm(Some("@ai-sdk/anthropic")),
            "qwen3.6-plus": with_npm(Some("@ai-sdk/anthropic")),
            "kept": with_npm(Some("@ai-sdk/anthropic")),
        }},
    })));
    let pick = |provider: &str, id: &str| {
        let found = catalog
            .find(provider, id)
            .unwrap_or_else(|| panic!("{provider}/{id}"));
        (found.api, found.base_url.clone())
    };
    assert_eq!(
        pick("opencode", "r"),
        (
            WireApi::OpenAiResponses,
            "https://opencode.ai/zen/v1".to_owned()
        )
    );
    assert_eq!(
        pick("opencode", "a"),
        (
            WireApi::AnthropicMessages,
            "https://opencode.ai/zen".to_owned()
        )
    );
    assert_eq!(
        pick("opencode", "g"),
        (
            WireApi::GoogleGenerativeAi,
            "https://opencode.ai/zen/v1".to_owned()
        )
    );
    assert_eq!(
        pick("opencode", "c"),
        (
            WireApi::OpenAiCompletions,
            "https://opencode.ai/zen/v1".to_owned()
        )
    );
    assert_eq!(
        pick("opencode-go", "kept"),
        (
            WireApi::AnthropicMessages,
            "https://opencode.ai/zen/go".to_owned()
        )
    );
    for id in ["minimax-m2.7", "qwen3.6-plus"] {
        assert_eq!(
            pick("opencode-go", id),
            (
                WireApi::OpenAiCompletions,
                "https://opencode.ai/zen/go/v1".to_owned()
            ),
            "{id}"
        );
    }
}

#[test]
fn cloudflare_gateway_ids_split_on_the_upstream_and_dashed_anthropic_ids() {
    let catalog = Catalog::from_models_dev(&dev(&json!({"cloudflare-ai-gateway": {"models": {
        "openai/gpt-x": model(true),
        "anthropic/claude-opus-5.5": model(true),
        "workers-ai/@cf/meta/llama": model(true),
        "mystery/other": model(true),
        "no-slash": model(true),
    }}})));
    let ids: Vec<(String, WireApi)> = only(&catalog, "cloudflare-ai-gateway")
        .into_iter()
        .map(|m| (m.id, m.api))
        .collect();
    assert_eq!(
        ids,
        [
            ("claude-opus-5-5".to_owned(), WireApi::AnthropicMessages),
            ("gpt-x".to_owned(), WireApi::OpenAiResponses),
            (
                "workers-ai/@cf/meta/llama".to_owned(),
                WireApi::OpenAiCompletions
            ),
        ]
    );
    let base = &catalog
        .find("cloudflare-ai-gateway", "gpt-x")
        .expect("gpt-x")
        .base_url;
    assert_eq!(
        base,
        "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai"
    );
}

#[test]
fn fireworks_splits_glm_and_kimi_k3_from_the_anthropic_compatible_rest() {
    let catalog = Catalog::from_models_dev(&dev(&json!({"fireworks-ai": {"models": {
        "accounts/fireworks/models/glm-5": model(true),
        "accounts/fireworks/models/kimi-k3": model(true),
        "accounts/fireworks/models/other": model(true),
    }}})));
    let apis: Vec<(String, WireApi, String)> = only(&catalog, "fireworks")
        .into_iter()
        .map(|m| {
            (
                m.id.rsplit('/').next().unwrap_or_default().to_owned(),
                m.api,
                m.base_url,
            )
        })
        .collect();
    assert_eq!(
        apis,
        [
            (
                "glm-5".to_owned(),
                WireApi::OpenAiCompletions,
                "https://api.fireworks.ai/inference/v1".to_owned()
            ),
            (
                "kimi-k3".to_owned(),
                WireApi::OpenAiCompletions,
                "https://api.fireworks.ai/inference/v1".to_owned()
            ),
            (
                "other".to_owned(),
                WireApi::AnthropicMessages,
                "https://api.fireworks.ai/inference".to_owned()
            ),
        ]
    );
}

#[test]
fn kimi_aliases_collapse_into_the_canonical_id_only_when_it_exists() {
    let with_canonical =
        Catalog::from_models_dev(&dev(&json!({"kimi-code-plan-global": {"models": {
            "kimi-for-coding": model(true), "k2p6": model(true), "k3": model(true),
        }}})));
    let ids: Vec<String> = only(&with_canonical, "kimi-coding")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, ["k3", "kimi-for-coding"]);
    let without = Catalog::from_models_dev(&dev(&json!({"kimi-code-plan-global": {"models": {
        "k2p6": model(true),
    }}})));
    let only_alias = only(&without, "kimi-coding");
    assert_eq!(
        (only_alias[0].id.as_str(), only_alias[0].name.as_str()),
        ("kimi-for-coding", "Kimi For Coding")
    );
}

#[test]
fn mistral_derives_a_cache_price_only_when_models_dev_has_none() {
    let mut priced = model(true);
    priced["cost"] = json!({"input": 2.0, "output": 6.0, "cache_read": 0.5});
    let mut unpriced = model(true);
    unpriced["cost"] = json!({"input": 2.0, "output": 6.0});
    let catalog = Catalog::from_models_dev(&dev(&json!({"mistral": {"models": {
        "a": priced, "b": unpriced,
    }}})));
    assert_eq!(
        catalog.find("mistral", "a").expect("a").cost.cache_read,
        0.5
    );
    assert_eq!(
        catalog.find("mistral", "b").expect("b").cost.cache_read,
        0.2
    );
}

#[test]
fn providers_without_a_source_and_an_empty_map_yield_nothing_and_every_source_is_listed_once() {
    assert_eq!(Catalog::from_models_dev(&Map::new()).models(), &[]);
    let catalog =
        Catalog::from_models_dev(&dev(&json!({"openrouter": {"models": {"m": model(true)}}})));
    assert!(
        catalog.models().is_empty(),
        "OpenRouter is filled from a live listing, not ported"
    );
    let providers = catalog_providers();
    let mut unique = providers.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), providers.len());
}

#[test]
fn deprecated_models_are_dropped_only_where_the_generator_drops_them() {
    let old = json!({"tool_call": true, "status": "deprecated"});
    let catalog = Catalog::from_models_dev(&dev(&json!({
        "openai": {"models": {"old": old.clone(), "new": model(true)}},
        "mistral": {"models": {"old": old.clone()}},
        "togetherai": {"models": {"old": old.clone(), "new": model(true)}},
        "opencode": {"models": {"old": old.clone()}},
        "xiaomi": {"models": {"old": old}},
    })));
    let ids = |provider: &str| {
        only(&catalog, provider)
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids("openai"),
        ["new", "old"],
        "kept: the generator keeps them for OpenAI"
    );
    assert_eq!(ids("mistral"), ["old"]);
    assert_eq!(ids("together"), ["new"], "dropped for Together");
    assert_eq!(ids("opencode"), Vec::<String>::new());
    assert_eq!(ids("xiaomi"), Vec::<String>::new());
}

#[test]
fn xai_leaves_out_the_models_the_generator_excludes() {
    let catalog = Catalog::from_models_dev(&dev(&json!({"xai": {"models": {
        "grok-3": model(true), "grok-code-fast-1": model(true), "grok-4": model(true),
    }}})));
    let ids: Vec<String> = only(&catalog, "xai").into_iter().map(|m| m.id).collect();
    assert_eq!(ids, ["grok-4"]);
}
