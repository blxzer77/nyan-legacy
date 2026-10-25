use serde_json::{Map, Value, json};

use super::{Cached, TTL_MS, path, read, write};

fn temp(name: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("nyan-models-{name}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn providers() -> Map<String, Value> {
    let Value::Object(map) = json!({"openai": {"models": {"m": {"tool_call": true}}}}) else {
        unreachable!()
    };
    map
}

#[test]
fn what_is_written_is_read_back_in_the_typescript_file_shape() {
    let dir = temp("roundtrip");
    write(&dir, &providers(), 1_700_000_000_000).expect("write");
    let raw: Value =
        serde_json::from_str(&std::fs::read_to_string(path(&dir)).expect("read")).expect("json");
    assert_eq!(raw["timestamp"], 1_700_000_000_000_u64);
    assert_eq!(raw["providers"]["openai"]["models"]["m"]["tool_call"], true);
    assert_eq!(
        read(&dir),
        Some(Cached {
            timestamp_ms: 1_700_000_000_000,
            providers: providers()
        })
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        ["models-dev-cache.json"],
        "no temporary file remains"
    );
}

#[test]
fn a_file_written_by_the_typescript_cli_is_accepted() {
    let dir = temp("typescript");
    std::fs::write(
        path(&dir),
        r#"{"timestamp":1760000000000,"providers":{"groq":{"models":{}}}}"#,
    )
    .expect("seed");
    let cached = read(&dir).expect("valid");
    assert_eq!(cached.timestamp_ms, 1_760_000_000_000);
    assert!(cached.providers.contains_key("groq"));
}

#[test]
fn a_missing_or_malformed_file_is_no_cache_rather_than_an_error() {
    let dir = temp("malformed");
    assert_eq!(read(&dir), None);
    for content in [
        "not json",
        "[]",
        r#"{"providers":{}}"#,
        r#"{"timestamp":"soon","providers":{}}"#,
        r#"{"timestamp":1,"providers":[]}"#,
        r#"{"timestamp":1}"#,
    ] {
        std::fs::write(path(&dir), content).expect("seed");
        assert_eq!(read(&dir), None, "{content}");
    }
}

#[test]
fn freshness_is_a_24_hour_window_that_a_future_timestamp_does_not_break() {
    let cached = Cached {
        timestamp_ms: 1_000,
        providers: Map::new(),
    };
    assert!(cached.is_fresh(1_000));
    assert!(cached.is_fresh(1_000 + TTL_MS - 1));
    assert!(!cached.is_fresh(1_000 + TTL_MS));
    assert!(
        cached.is_fresh(0),
        "a clock that went backwards keeps the copy usable"
    );
}
