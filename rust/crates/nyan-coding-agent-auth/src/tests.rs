use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::{ApiKeyCredential, AuthError, AuthStore, Credential};

fn temp(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("nyan-auth-{name}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn file_json(store: &AuthStore) -> Value {
    serde_json::from_str(&std::fs::read_to_string(store.path()).expect("read")).expect("json")
}

#[test]
fn a_missing_file_holds_no_credentials() {
    let dir = temp("missing");
    let store = AuthStore::new(&dir.join("not-created-yet"));
    assert_eq!(store.credential("cliproxyapi"), Ok(None));
    assert_eq!(store.providers(), Ok(Vec::new()));
    assert!(
        !dir.join("not-created-yet").exists(),
        "reading creates nothing"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_api_key_round_trips_with_its_settings_in_the_typescript_shape() {
    let dir = temp("roundtrip");
    let store = AuthStore::new(&dir);
    let settings = env(&[("CLIPROXYAPI_BASE_URL", "http://127.0.0.1:8317")]);
    store
        .save_api_key("cliproxyapi", "sk-test", &settings)
        .expect("save");
    assert_eq!(
        store.credential("cliproxyapi"),
        Ok(Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("sk-test".to_owned()),
            env: settings,
        })))
    );
    assert_eq!(
        file_json(&store),
        json!({"cliproxyapi": {
            "type": "api_key",
            "key": "sk-test",
            "env": {"CLIPROXYAPI_BASE_URL": "http://127.0.0.1:8317"}
        }})
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn saving_one_provider_leaves_the_other_entries_exactly_as_they_were() {
    let dir = temp("preserve");
    let store = AuthStore::new(&dir);
    let oauth = json!({"type": "oauth", "access": "a", "refresh": "r", "expires": 123, "accountId": "acct"});
    let other = json!({"type": "api_key", "key": "k", "env": {"CLOUDFLARE_ACCOUNT_ID": "x"}});
    std::fs::write(
        store.path(),
        json!({"openai-codex": oauth, "cloudflare": other}).to_string(),
    )
    .expect("seed");
    store
        .save_api_key("cliproxyapi", "sk-new", &BTreeMap::new())
        .expect("save");
    let after = file_json(&store);
    assert_eq!(after["openai-codex"], oauth, "extra oauth fields survive");
    assert_eq!(after["cloudflare"], other);
    assert_eq!(after["cliproxyapi"]["key"], "sk-new");
    assert_eq!(
        store.credential("openai-codex"),
        Ok(Some(Credential::OAuth))
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_invalid_file_is_reported_and_never_overwritten() {
    let dir = temp("invalid");
    let store = AuthStore::new(&dir);
    for content in [
        "not json".to_owned(),
        "[]".to_owned(),
        json!({"cliproxyapi": {"type": "api_key", "key": 5}}).to_string(),
        json!({"x": {"type": "mystery"}}).to_string(),
        json!({"x": {"type": "oauth", "access": "a"}}).to_string(),
    ] {
        std::fs::write(store.path(), &content).expect("seed");
        assert_eq!(
            store.credential("cliproxyapi").expect_err("invalid").code(),
            "auth_invalid",
            "{content}"
        );
        let error = store
            .save_api_key("cliproxyapi", "sk", &BTreeMap::new())
            .expect_err("must refuse");
        assert!(
            matches!(error, AuthError::Invalid { .. }),
            "{content}: {error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(store.path()).expect("read"),
            content
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn remove_reports_whether_a_credential_existed() {
    let dir = temp("remove");
    let store = AuthStore::new(&dir);
    store
        .save_api_key("a", "1", &BTreeMap::new())
        .expect("save");
    store
        .save_api_key("b", "2", &BTreeMap::new())
        .expect("save");
    assert_eq!(store.remove("a"), Ok(true));
    assert_eq!(store.remove("a"), Ok(false));
    assert_eq!(store.providers(), Ok(vec!["b".to_owned()]));
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn the_credential_file_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp("mode");
    let store = AuthStore::new(&dir);
    store
        .save_api_key("a", "secret", &BTreeMap::new())
        .expect("save");
    let mode = std::fs::metadata(store.path())
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_held_lock_blocks_a_write_and_a_stale_lock_does_not() {
    let dir = temp("lock");
    let store = AuthStore::new(&dir).with_lock_wait(Duration::from_millis(100));
    let lock = dir.join("auth.json.lock");
    std::fs::write(&lock, "").expect("lock");
    let error = store
        .save_api_key("a", "1", &BTreeMap::new())
        .expect_err("locked");
    assert_eq!(error.code(), "auth_locked");
    assert!(!dir.join("auth.json").exists(), "nothing was written");

    let old = std::time::SystemTime::now() - Duration::from_secs(120);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .and_then(|file| file.set_modified(old))
        .expect("age the lock");
    store
        .save_api_key("a", "1", &BTreeMap::new())
        .expect("stale lock is broken");
    assert!(!lock.exists(), "the lock is released after a write");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn concurrent_logins_for_different_providers_all_survive() {
    let dir = temp("concurrent");
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                AuthStore::new(&dir)
                    .save_api_key(
                        &format!("provider-{index}"),
                        &format!("key-{index}"),
                        &BTreeMap::new(),
                    )
                    .expect("save");
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread");
    }
    assert_eq!(
        AuthStore::new(&dir).providers().expect("providers").len(),
        8
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        ["auth.json"],
        "no lock or temporary files remain"
    );
    let _ = std::fs::remove_dir_all(dir);
}
