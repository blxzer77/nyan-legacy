use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::{ApiKeyCredential, AuthError, AuthStore, Credential, OAuthCredential};

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
    let Ok(Some(Credential::OAuth(login))) = store.credential("openai-codex") else {
        panic!("the oauth entry must parse");
    };
    assert_eq!(
        (
            login.access.as_str(),
            login.refresh.as_str(),
            login.expires_ms
        ),
        ("a", "r", 123)
    );
    assert_eq!(login.extra_str("accountId"), Some("acct"));
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
fn a_held_lock_blocks_a_write_and_a_leftover_lock_file_does_not() {
    let dir = temp("lock");
    let store = AuthStore::new(&dir).with_lock_wait(Duration::from_millis(100));
    let lock = dir.join("auth.json.flock");
    // A lock file nobody holds, as a killed process leaves behind, blocks nothing.
    std::fs::write(&lock, "").expect("leftover lock file");
    store
        .save_api_key("a", "1", &BTreeMap::new())
        .expect("a leftover lock file is not a held lock");

    // A lock that is held blocks the write until the holder lets go.
    let holder = std::fs::File::options()
        .write(true)
        .open(&lock)
        .expect("open lock");
    holder.try_lock().expect("take the lock");
    let error = store
        .save_api_key("b", "2", &BTreeMap::new())
        .expect_err("locked");
    assert_eq!(error.code(), "auth_locked");
    assert!(
        !file_json(&store)
            .as_object()
            .expect("object")
            .contains_key("b"),
        "nothing was written while the lock was held"
    );
    drop(holder);
    store
        .save_api_key("b", "2", &BTreeMap::new())
        .expect("the lock was released");
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
    let mut leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    leftovers.sort();
    assert_eq!(
        leftovers,
        ["auth.json", "auth.json.flock"],
        "only the credential file and its lock file remain, no temporary files"
    );
    let _ = std::fs::remove_dir_all(dir);
}

fn login(access: &str, refresh: &str, expires_ms: u64) -> OAuthCredential {
    OAuthCredential {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires_ms,
        extra: BTreeMap::from([("accountId".to_owned(), json!("acct-1"))]),
    }
}

#[test]
fn an_oauth_login_round_trips_in_the_typescript_shape_with_its_extra_fields() {
    let dir = temp("oauth-roundtrip");
    let store = AuthStore::new(&dir);
    store
        .save_oauth("openai-codex", &login("tok", "ref", 1_800_000_000_000))
        .expect("save");
    assert_eq!(
        file_json(&store),
        json!({"openai-codex": {"type": "oauth", "access": "tok", "refresh": "ref",
                                "expires": 1_800_000_000_000_u64, "accountId": "acct-1"}})
    );
    assert_eq!(
        store.credential("openai-codex"),
        Ok(Some(Credential::OAuth(login(
            "tok",
            "ref",
            1_800_000_000_000
        ))))
    );
    assert!(!format!("{:?}", login("secret-access", "secret-refresh", 1)).contains("secret"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_valid_login_is_returned_without_calling_the_refresher_and_an_expired_one_is_renewed_and_saved()
{
    let dir = temp("refresh");
    let store = AuthStore::new(&dir);
    store
        .save_oauth("p", &login("old", "r1", 10_000))
        .expect("save");

    let untouched = store
        .refresh_oauth("p", 1_000, 100, |_| {
            panic!("a valid login must not be refreshed")
        })
        .expect("valid");
    assert_eq!(untouched.access, "old");

    // Inside the margin counts as expired.
    let renewed = store
        .refresh_oauth("p", 9_950, 100, |current| {
            assert_eq!(current.refresh, "r1");
            Ok(login("new", "r2", 99_000))
        })
        .expect("renewed");
    assert_eq!(
        (renewed.access.as_str(), renewed.refresh.as_str()),
        ("new", "r2")
    );
    assert_eq!(
        store.credential("p"),
        Ok(Some(Credential::OAuth(login("new", "r2", 99_000))))
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failed_refresh_changes_nothing_and_a_missing_login_is_named() {
    let dir = temp("refresh-fail");
    let store = AuthStore::new(&dir);
    store
        .save_oauth("p", &login("old", "r1", 10))
        .expect("save");
    let before = std::fs::read_to_string(store.path()).expect("read");
    let error = store
        .refresh_oauth("p", 1_000, 0, |_| Err("invalid_grant".to_owned()))
        .expect_err("refresher failed");
    assert_eq!(error.code(), "auth_refresh_failed");
    assert!(error.to_string().contains("invalid_grant"));
    assert_eq!(std::fs::read_to_string(store.path()).expect("read"), before);
    assert!(
        !dir.join("auth.json.lock").exists(),
        "the lock is released after a failure"
    );

    let missing = store.refresh_oauth("nobody", 1_000, 0, |_| panic!("no login to refresh"));
    assert_eq!(missing.expect_err("none").code(), "auth_not_logged_in");
    store
        .save_api_key("keyed", "k", &BTreeMap::new())
        .expect("save");
    assert_eq!(
        store
            .refresh_oauth("keyed", 1_000, 0, |_| panic!("an API key is not refreshed"))
            .expect_err("not oauth")
            .code(),
        "auth_not_logged_in"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn two_processes_expiring_together_refresh_once_because_the_second_rereads_under_the_lock() {
    let dir = temp("refresh-race");
    AuthStore::new(&dir)
        .save_oauth("p", &login("old", "r1", 10))
        .expect("save");
    let refreshes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let dir = dir.clone();
            let refreshes = std::sync::Arc::clone(&refreshes);
            std::thread::spawn(move || {
                AuthStore::new(&dir)
                    .refresh_oauth("p", 1_000, 0, |_| {
                        refreshes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(30));
                        Ok(login("new", "r2", 1_000_000))
                    })
                    .expect("refresh")
                    .access
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().expect("thread"), "new");
    }
    assert_eq!(
        refreshes.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a refresh token is single-use: a second refresh with the old one would fail"
    );
    let _ = std::fs::remove_dir_all(dir);
}
