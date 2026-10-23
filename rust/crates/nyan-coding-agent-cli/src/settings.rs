//! The saved default model in `settings.json`.
//!
//! `defaultProvider` and `defaultModel` are the keys the TypeScript CLI used,
//! and every other key in the file is left as it was. A file that does not
//! parse is reported and never overwritten: only its owner can say whether the
//! content is recoverable. There is no cross-process lock; two writers at the
//! same instant keep whichever renamed last.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// A provider and model chosen by the user.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultModel {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model: String,
}

fn settings_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("settings.json")
}

fn read_object(path: &Path) -> Result<Map<String, Value>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(format!(
            "{} is not a JSON object; fix or remove it, nothing was changed",
            path.display()
        )),
        Err(error) => Err(format!(
            "{} is not valid JSON ({error}); fix or remove it, nothing was changed",
            path.display()
        )),
    }
}

/// Reads the saved default model, if both halves are set.
///
/// # Errors
///
/// Returns a message when `settings.json` cannot be read or parsed.
pub fn read_default_model(agent_dir: &Path) -> Result<Option<DefaultModel>, String> {
    let object = read_object(&settings_path(agent_dir))?;
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Ok(text("defaultProvider")
        .zip(text("defaultModel"))
        .map(|(provider, model)| DefaultModel { provider, model }))
}

/// Saves the default model, keeping every other setting.
///
/// # Errors
///
/// Returns a message when `settings.json` cannot be read, parsed, or written.
pub fn write_default_model(agent_dir: &Path, default: &DefaultModel) -> Result<(), String> {
    let path = settings_path(agent_dir);
    let mut object = read_object(&path)?;
    object.insert(
        "defaultProvider".to_owned(),
        Value::String(default.provider.clone()),
    );
    object.insert(
        "defaultModel".to_owned(),
        Value::String(default.model.clone()),
    );
    fs::create_dir_all(agent_dir)
        .map_err(|error| format!("could not create {}: {error}", agent_dir.display()))?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut text =
        serde_json::to_string_pretty(&Value::Object(object)).map_err(|error| error.to_string())?;
    text.push('\n');
    let result = fs::File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, &path));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("could not write {}: {error}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DefaultModel, read_default_model, write_default_model};

    fn temp(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nyan-settings-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn model(provider: &str, id: &str) -> DefaultModel {
        DefaultModel {
            provider: provider.to_owned(),
            model: id.to_owned(),
        }
    }

    #[test]
    fn a_missing_file_has_no_default_and_writing_creates_it() {
        let dir = temp("missing").join("agent");
        assert_eq!(read_default_model(&dir), Ok(None));
        write_default_model(&dir, &model("cliproxyapi", "gpt-6")).expect("write");
        assert_eq!(
            read_default_model(&dir),
            Ok(Some(model("cliproxyapi", "gpt-6")))
        );
    }

    #[test]
    fn writing_keeps_every_other_setting() {
        let dir = temp("keep");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"theme":"dark","packages":["a","b"],"defaultModel":"old","compaction":{"enabled":true}}"#,
        )
        .expect("seed");
        write_default_model(&dir, &model("p", "m")).expect("write");
        let after: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("settings.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["packages"], serde_json::json!(["a", "b"]));
        assert_eq!(after["compaction"], serde_json::json!({"enabled": true}));
        assert_eq!(after["defaultProvider"], "p");
        assert_eq!(after["defaultModel"], "m");
    }

    #[test]
    fn half_a_default_counts_as_none() {
        let dir = temp("half");
        std::fs::write(dir.join("settings.json"), r#"{"defaultModel":"m"}"#).expect("seed");
        assert_eq!(read_default_model(&dir), Ok(None));
    }

    #[test]
    fn a_file_that_does_not_parse_is_reported_and_left_alone() {
        let dir = temp("broken");
        for content in ["{ nope", "[1]"] {
            std::fs::write(dir.join("settings.json"), content).expect("seed");
            assert!(
                read_default_model(&dir)
                    .expect_err("read")
                    .contains("nothing was changed")
            );
            assert!(write_default_model(&dir, &model("p", "m")).is_err());
            assert_eq!(
                std::fs::read_to_string(dir.join("settings.json")).expect("read"),
                content
            );
        }
    }
}
