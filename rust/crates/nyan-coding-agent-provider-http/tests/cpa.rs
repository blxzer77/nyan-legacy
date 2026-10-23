//! The proxy's endpoint rules and catalog, against a loopback server.

use nyan_coding_agent_provider_http::cpa::{
    CatalogError, CatalogModel, Endpoints, fetch_catalog, parse_catalog, resolve_endpoints,
};
use tokio::runtime::Runtime;

mod common;
use common::serve;

fn endpoints(input: &str) -> Endpoints {
    resolve_endpoints(input).unwrap_or_else(|error| panic!("{input}: {error}"))
}

#[test]
fn any_url_the_user_pastes_reduces_to_the_same_root() {
    let expected = Endpoints {
        root: "http://127.0.0.1:8317".to_owned(),
        inference_base_url: "http://127.0.0.1:8317/backend-api/codex".to_owned(),
        models_url: "http://127.0.0.1:8317/v1/models?client_version=pi".to_owned(),
    };
    for input in [
        "127.0.0.1:8317",
        "http://127.0.0.1:8317",
        "http://127.0.0.1:8317/",
        "  http://127.0.0.1:8317/v1  ",
        "http://127.0.0.1:8317/v1/models",
        "http://127.0.0.1:8317/backend-api",
        "http://127.0.0.1:8317/backend-api/codex",
        "http://127.0.0.1:8317/backend-api/codex/responses",
        "HTTP://127.0.0.1:8317",
    ] {
        assert_eq!(endpoints(input), expected, "{input}");
    }
}

#[test]
fn a_path_prefix_and_https_are_kept() {
    assert_eq!(
        endpoints("https://proxy.example.com/cpa/v1"),
        Endpoints {
            root: "https://proxy.example.com/cpa".to_owned(),
            inference_base_url: "https://proxy.example.com/cpa/backend-api/codex".to_owned(),
            models_url: "https://proxy.example.com/cpa/v1/models?client_version=pi".to_owned(),
        }
    );
    assert_eq!(
        endpoints("https://proxy.example.com:443").root,
        "https://proxy.example.com"
    );
}

#[test]
fn empty_and_malformed_inputs_are_errors() {
    assert_eq!(resolve_endpoints("   "), Err("baseUrl is empty".to_owned()));
    assert!(resolve_endpoints("http://").is_err());
    assert!(resolve_endpoints("http://exa mple.com").is_err());
}

#[test]
fn catalog_entries_map_to_models_with_the_documented_fallbacks() {
    let body = r#"{"models":[
        {"slug":"gpt-6","display_name":"GPT 6","context_window":500000,"max_context_window":900000,
         "max_output_tokens":128000,"input_modalities":["text","image"],
         "supported_reasoning_levels":[{"effort":"Low"},"high",{"effort":"low"}]},
        {"id":"plain","name":"Plain"},
        {"slug":"secret","visibility":"hide"},
        {"slug":"  "},
        {"slug":"no-reasoning","supported_reasoning_levels":["none"]}
    ]}"#;
    assert_eq!(
        parse_catalog(body),
        [
            CatalogModel {
                id: "gpt-6".to_owned(),
                name: "GPT 6".to_owned(),
                context_window: 500_000,
                max_tokens: 128_000,
                reasoning: true,
                reasoning_levels: vec!["low".to_owned(), "high".to_owned()],
                image_input: true,
            },
            CatalogModel {
                id: "plain".to_owned(),
                name: "Plain".to_owned(),
                context_window: 128_000,
                max_tokens: 16_384,
                reasoning: false,
                reasoning_levels: Vec::new(),
                image_input: false,
            },
            CatalogModel {
                id: "no-reasoning".to_owned(),
                name: "no-reasoning".to_owned(),
                context_window: 128_000,
                max_tokens: 16_384,
                reasoning: false,
                reasoning_levels: vec!["none".to_owned()],
                image_input: false,
            },
        ]
    );
}

#[test]
fn the_three_response_shapes_and_junk_bodies_are_handled() {
    let one = r#"{"slug":"m"}"#;
    assert_eq!(parse_catalog(&format!("[{one}]")).len(), 1);
    assert_eq!(parse_catalog(&format!(r#"{{"models":[{one}]}}"#)).len(), 1);
    assert_eq!(parse_catalog(&format!(r#"{{"data":[{one}]}}"#)).len(), 1);
    for junk in ["", "not json", "{}", "[]", "42", r#"{"models":"x"}"#] {
        assert_eq!(parse_catalog(junk), Vec::new(), "{junk:?}");
    }
}

#[test]
fn fetching_sends_the_key_and_the_client_version_and_maps_the_models() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(0, br#"{"models":[{"slug":"m1"},{"slug":"m2"}]}"#.to_vec())],
        false,
    );
    let url = format!("http://{}/v1/models?client_version=pi", server.address);
    let models = runtime
        .block_on(fetch_catalog(runtime.handle(), &url, "sk-secret"))
        .expect("catalog");
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["m1", "m2"]
    );
    let request = server.captured.lock().expect("lock").to_ascii_lowercase();
    assert!(
        request.starts_with("get /v1/models?client_version=pi http/1.1"),
        "{request}"
    );
    assert!(
        request.contains("authorization: bearer sk-secret"),
        "{request}"
    );
}

#[test]
fn a_rejected_key_and_an_unreachable_proxy_are_distinct_errors() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "401 Unauthorized",
        vec![(0, b"invalid api key".to_vec())],
        false,
    );
    let url = format!("http://{}/v1/models?client_version=pi", server.address);
    let error = runtime
        .block_on(fetch_catalog(runtime.handle(), &url, "bad"))
        .expect_err("rejected");
    assert_eq!(
        error,
        CatalogError::Http {
            status: 401,
            body: "invalid api key".to_owned()
        }
    );
    assert!(error.is_unauthorized());

    let unreachable = runtime
        .block_on(fetch_catalog(
            runtime.handle(),
            "http://127.0.0.1:1/v1/models",
            "k",
        ))
        .expect_err("nothing listens on port 1");
    assert!(
        matches!(unreachable, CatalogError::Transport { .. }),
        "{unreachable:?}"
    );
    assert!(!unreachable.is_unauthorized());
}
