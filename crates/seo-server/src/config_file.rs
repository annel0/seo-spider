//! Settings as a TOML file, the way the CLI takes them (`crawl --preset P
//! --config file.toml`): the settings form's preset and changes written
//! out, and a file read back over a preset as the form's changes.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use seo_crawl::CrawlConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::crawls::{self, StartRequest};
use crate::{ApiError, ApiResult};

/// Keys that belong to one crawl, not to settings carried between them.
const OWN: &[&str] = &["start_urls", "render_decision"];

fn bad(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, error.to_string())
}

#[derive(Deserialize)]
pub(crate) struct Settings {
    preset: String,
    /// The form's changes, a JSON merge patch over the preset.
    config: Option<Value>,
}

/// The settings as a TOML file for the CLI.
pub(crate) async fn export(Json(settings): Json<Settings>) -> Result<Response, ApiError> {
    let request = StartRequest {
        // The URL is the command line's; this one only lets the settings build.
        start_url: "https://example.com/".into(),
        preset: settings.preset.clone(),
        max_urls: None,
        render_threads: None,
        config: settings.config,
    };
    let config = crawls::build_config(&request).map_err(|error| bad(format!("{error:#}")))?;
    let mut value = toml::Value::try_from(&config).map_err(bad)?;
    if let toml::Value::Table(table) = &mut value {
        for key in OWN {
            table.remove(*key);
        }
    }
    let text = format!(
        "# seo-spider crawl https://example.com/ --preset {} --config this-file.toml\n\n{}",
        settings.preset,
        toml::to_string_pretty(&value).map_err(bad)?
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/toml; charset=utf-8"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"seo-spider.toml\""),
        ],
        text,
    )
        .into_response())
}

#[derive(Deserialize)]
pub(crate) struct File {
    preset: String,
    toml: String,
}

#[derive(Serialize)]
pub(crate) struct Imported {
    /// The file's settings as the form's changes to the preset.
    patch: Value,
}

/// Each table of `from` over the same one of `into`, as the CLI merges.
fn merge(into: &mut toml::Value, from: toml::Value) {
    match (into, from) {
        (toml::Value::Table(into), toml::Value::Table(from)) => {
            for (key, value) in from {
                match into.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        into.insert(key, value);
                    }
                }
            }
        }
        (into, from) => *into = from,
    }
}

/// The JSON merge patch that turns `base` into `next` (RFC 7396).
fn difference(base: &Value, next: &Value) -> Option<Value> {
    match (base, next) {
        (Value::Object(base), Value::Object(next)) => {
            let mut patch = serde_json::Map::new();
            for (key, value) in next {
                match base.get(key) {
                    Some(before) => {
                        if let Some(changed) = difference(before, value) {
                            patch.insert(key.clone(), changed);
                        }
                    }
                    None => {
                        patch.insert(key.clone(), value.clone());
                    }
                }
            }
            for key in base.keys().filter(|key| !next.contains_key(*key)) {
                patch.insert(key.clone(), Value::Null);
            }
            (!patch.is_empty()).then_some(Value::Object(patch))
        }
        (base, next) => (base != next).then(|| next.clone()),
    }
}

/// A TOML file over the preset, as changes for the form.
pub(crate) async fn import(Json(file): Json<File>) -> ApiResult<Imported> {
    let preset = CrawlConfig::preset(&file.preset).ok_or_else(|| bad(format!("неизвестный пресет {:?}", file.preset)))?;
    let read: toml::Value = toml::from_str(&file.toml).map_err(|error| bad(format!("TOML: {error}")))?;
    let mut merged = toml::Value::try_from(&preset).map_err(bad)?;
    merge(&mut merged, read);
    let config: CrawlConfig = merged.try_into().map_err(|error| bad(format!("настройки: {error}")))?;
    let (base, next) = (serde_json::to_value(&preset).map_err(bad)?, serde_json::to_value(&config).map_err(bad)?);
    let mut patch = difference(&base, &next).unwrap_or_else(|| Value::Object(Default::default()));
    if let Value::Object(object) = &mut patch {
        for key in OWN {
            object.remove(*key);
        }
    }
    Ok(Json(Imported { patch }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_file_goes_out_and_comes_back_as_the_same_changes() {
        let changes = serde_json::json!({"max_urls": 500, "http": {"concurrency": 3}, "render": null});
        let Ok(response) = export(Json(Settings { preset: "js".into(), config: Some(changes.clone()) })).await else { panic!("not exported") };
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.starts_with("# seo-spider crawl") && text.contains("max_urls = 500") && !text.contains("start_urls"), "{text}");
        // Read over a preset, the file is its changes to it.
        let Ok(Json(back)) = import(Json(File { preset: "seo".into(), toml: text })).await else { panic!("not imported") };
        assert_eq!(back.patch["max_urls"], 500);
        assert_eq!(back.patch["http"], serde_json::json!({"concurrency": 3}));
        assert!(back.patch.get("start_urls").is_none());
        let Err(error) = import(Json(File { preset: "seo".into(), toml: "[http]\nnope = 1".into() })).await else { panic!("a bad key read") };
        assert!(error.1.contains("nope"), "{}", error.1);
    }

    #[test]
    fn the_difference_is_a_merge_patch() {
        let base = serde_json::json!({"a": 1, "b": {"c": 2, "d": 3}, "e": [1], "f": 1});
        let next = serde_json::json!({"a": 1, "b": {"c": 2, "d": 4}, "e": [1, 2], "g": true});
        assert_eq!(difference(&base, &next), Some(serde_json::json!({"b": {"d": 4}, "e": [1, 2], "f": null, "g": true})));
        assert_eq!(difference(&base, &base), None);
    }
}
