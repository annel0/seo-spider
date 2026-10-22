//! The crawl the server runs: one at a time, on its own thread and tokio
//! runtime (render threads and V8 stay out of the server's), steered by
//! its `CrawlControl` and followed through its progress and phase.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use seo_crawl::{CrawlConfig, CrawlControl, CrawlProgress};
use seo_store::Store;
use serde::Deserialize;
use tokio::sync::watch;
use url::Url;

/// Where a crawl the server started is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Crawling,
    /// The crawl is over and its cross-page checks run.
    Checking,
    Finished,
    Failed(String),
}

/// A crawl the server started, while the server runs.
#[derive(Clone)]
pub struct Running {
    pub id: i64,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    pub progress: watch::Receiver<CrawlProgress>,
    pub phase: watch::Receiver<Phase>,
}

impl Running {
    pub fn pause(&self, paused: bool) {
        self.pause.store(paused, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // A paused crawl stops too.
        self.pause.store(false, Ordering::Relaxed);
    }

    pub fn is_active(&self) -> bool {
        matches!(*self.phase.borrow(), Phase::Crawling | Phase::Checking)
    }
}

/// What starts a crawl: a URL, a preset, and changes to the preset's
/// settings, either as the few common ones or as a JSON merge patch over
/// the whole configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub start_url: String,
    #[serde(default = "default_preset")]
    pub preset: String,
    pub max_urls: Option<usize>,
    /// 0 turns rendering off.
    pub render_threads: Option<usize>,
    /// Merged over the preset's settings (RFC 7396).
    pub config: Option<serde_json::Value>,
}

fn default_preset() -> String {
    "seo".into()
}

/// The configuration a request describes, built the way the CLI builds it:
/// preset, then changes.
pub fn build_config(request: &StartRequest) -> Result<CrawlConfig> {
    let mut config =
        CrawlConfig::preset(&request.preset).ok_or_else(|| anyhow!("неизвестный пресет {:?}", request.preset))?;
    if let Some(patch) = &request.config {
        let mut value = serde_json::to_value(&config)?;
        merge(&mut value, patch);
        config = serde_json::from_value(value).map_err(|error| anyhow!("настройки: {error}"))?;
    }
    let start = Url::parse(request.start_url.trim()).map_err(|error| anyhow!("адрес {:?}: {error}", request.start_url))?;
    if !matches!(start.scheme(), "http" | "https") {
        return Err(anyhow!("адрес должен начинаться с http:// или https://"));
    }
    config.start_urls = vec![start];
    if let Some(max) = request.max_urls {
        config.max_urls = max;
    }
    match request.render_threads {
        Some(0) => config.render = None,
        Some(threads) => config.render.get_or_insert_with(Default::default).threads = threads,
        None => {}
    }
    if let Some(render) = &mut config.render {
        render.allow_private_network |= config.start_urls.iter().any(seo_crawl::is_private_host);
    }
    Ok(config)
}

/// JSON merge patch: objects merge key by key, `null` removes, anything
/// else replaces.
fn merge(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    target.remove(key);
                } else {
                    merge(target.entry(key.clone()).or_insert(serde_json::Value::Null), value);
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

/// What may change when a crawl is continued.
#[derive(Debug, Default, Deserialize)]
pub struct ContinueRequest {
    /// A higher limit than the crawl had.
    pub max_urls: Option<usize>,
    /// Render threads, for a crawl that renders.
    pub render_threads: Option<usize>,
}

/// Starts a crawl into `db` and returns once it is recorded (its id known)
/// or has failed to start.
pub async fn start(db: PathBuf, config: CrawlConfig) -> Result<Running> {
    launch(db, move |store, control| seo_crawl::crawl_with(config, store, control)).await
}

/// Goes on with an interrupted or stopped crawl of `db`, with its own
/// settings and what the request changes.
pub async fn resume(db: PathBuf, crawl_id: i64, request: ContinueRequest) -> Result<Running> {
    let text = Store::open(&db)?.crawl_config(crawl_id).map_err(|_| anyhow!("нет обхода {crawl_id}"))?;
    let mut config: CrawlConfig = serde_json::from_str(&text).map_err(|error| anyhow!("настройки обхода: {error}"))?;
    if let Some(max_urls) = request.max_urls {
        config.max_urls = max_urls;
    }
    if let (Some(threads), Some(render)) = (request.render_threads, config.render.as_mut()) {
        render.threads = threads.max(1);
    }
    launch(db, move |store, control| seo_crawl::continue_with(config, store, crawl_id, control)).await
}

/// Runs a crawl on a thread and runtime of its own, then its checks, and
/// returns once it is recorded (its id known) or has failed to start.
async fn launch<F>(db: PathBuf, run: impl FnOnce(Store, CrawlControl) -> F + Send + 'static) -> Result<Running>
where
    F: std::future::Future<Output = Result<(Store, seo_crawl::CrawlSummary)>>,
{
    let stop = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(false));
    let (progress_sender, mut progress) = watch::channel(CrawlProgress::default());
    let (phase_sender, mut phase) = watch::channel(Phase::Crawling);
    let control = CrawlControl {
        stop: stop.clone(),
        pause: pause.clone(),
        progress: Some(progress_sender),
        live_issues: true,
    };
    std::thread::Builder::new().name("crawl".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                phase_sender.send_replace(Phase::Failed(error.to_string()));
                return;
            }
        };
        let crawled = runtime.block_on(async {
            let store = Store::open(&db)?;
            run(store, control).await
        });
        match crawled {
            Ok((mut store, summary)) => {
                phase_sender.send_replace(Phase::Checking);
                let checked = seo_crawl::run_checks(&mut store, summary.crawl_id)
                    .and_then(|_| store.ensure_view_indexes());
                phase_sender.send_replace(match checked {
                    Ok(()) => Phase::Finished,
                    Err(error) => Phase::Failed(format!("проверки: {error:#}")),
                });
            }
            Err(error) => {
                phase_sender.send_replace(Phase::Failed(format!("{error:#}")));
            }
        }
        // Render threads hold V8 isolates; give them a moment, not forever.
        runtime.shutdown_timeout(Duration::from_secs(2));
    })?;
    let id = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Phase::Failed(error) = &*phase.borrow_and_update() {
                return Err(anyhow!("{error}"));
            }
            let id = progress.borrow_and_update().crawl_id;
            if id > 0 {
                return Ok(id);
            }
            tokio::select! {
                changed = progress.changed() => changed.map_err(|_| anyhow!("обход завершился, не начавшись"))?,
                changed = phase.changed() => changed.map_err(|_| anyhow!("обход завершился, не начавшись"))?,
            }
        }
    })
    .await
    .map_err(|_| anyhow!("обход не начался за минуту"))??;
    Ok(Running { id, stop, pause, progress, phase })
}

#[cfg(test)]
mod tests {
    use super::*;
    use seo_crawl::RenderSettings;

    fn request(config: Option<serde_json::Value>) -> StartRequest {
        StartRequest {
            start_url: "https://example.com/".into(),
            preset: "seo".into(),
            max_urls: Some(50),
            render_threads: None,
            config,
        }
    }

    #[test]
    fn settings_build_on_the_preset() {
        let config = build_config(&request(None)).unwrap();
        assert_eq!(config.max_urls, 50);
        assert!(config.render.is_none());
        let patched = build_config(&request(Some(serde_json::json!({
            "http": {"concurrency": 3, "headers": {"Cookie": "a=1"}},
            "custom": {"search": [{"name": "x", "text": "y"}]}
        }))))
        .unwrap();
        assert_eq!(patched.http.concurrency, 3);
        assert_eq!(patched.http.headers.get("Cookie").map(String::as_str), Some("a=1"));
        assert_eq!(patched.custom.search.len(), 1);
        // The rest of the preset stays.
        assert_eq!(patched.http.retries, CrawlConfig::default().http.retries);
        let rendered = build_config(&StartRequest { render_threads: Some(4), ..request(None) }).unwrap();
        assert_eq!(rendered.render.unwrap().threads, 4);
        // The settings form turns a preset's rendering off with null, and on
        // with an object that the defaults fill in.
        let off = build_config(&StartRequest { preset: "js".into(), ..request(Some(serde_json::json!({"render": null}))) }).unwrap();
        assert!(off.render.is_none());
        let on = build_config(&request(Some(serde_json::json!({"render": {"locale": "ru-RU"}})))).unwrap().render.unwrap();
        assert_eq!((on.locale.as_deref(), on.threads), (Some("ru-RU"), RenderSettings::default().threads));
    }

    #[test]
    fn bad_requests_say_what_is_wrong() {
        let unknown = build_config(&request(Some(serde_json::json!({"http": {"nope": 1}})))).unwrap_err();
        assert!(unknown.to_string().contains("nope"), "{unknown}");
        let preset = build_config(&StartRequest { preset: "fast".into(), ..request(None) }).unwrap_err();
        assert!(preset.to_string().contains("пресет"), "{preset}");
        let scheme = build_config(&StartRequest { start_url: "ftp://example.com/".into(), ..request(None) }).unwrap_err();
        assert!(scheme.to_string().contains("http"), "{scheme}");
    }
}
