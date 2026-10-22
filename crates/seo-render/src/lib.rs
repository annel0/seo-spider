//! A pool of render workers on the embedded obscura engine.
//!
//! Each worker is an OS thread with its own current-thread runtime and one
//! obscura `Page`, the way obscura's CDP server runs a connection. Pages are
//! not `Send`, so URLs go in and results come out over channels. A page is
//! replaced every `recycle_after` renders so per-page leaks cannot pile up.

use std::sync::Arc;
use std::time::{Duration, Instant};

use obscura_browser::{BrowserContext, Page, WaitUntil};
pub use obscura_browser::SettlePolicy;
use serde::Serialize;
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct RenderConfig {
    pub threads: usize,
    /// When the content counts as settled after DOMContentLoaded.
    pub settle: SettlePolicy,
    /// Upper bound for the navigation itself (up to DOMContentLoaded).
    pub navigation_timeout: Duration,
    pub user_agent: Option<String>,
    /// Replace a worker's page after this many renders.
    pub recycle_after: usize,
    /// Let pages reach loopback and private addresses. obscura blocks them
    /// by default so a public page cannot probe the crawler's own network.
    pub allow_private_network: bool,
    /// Stall limit for requests to other sites than the page's. A tracker
    /// that never answers otherwise holds a page that defers its script for
    /// obscura's 10 s stall limit. `None` keeps that limit.
    pub third_party_timeout: Option<Duration>,
    /// Skip the tracker and ad hosts of obscura's built-in list.
    pub block_trackers: bool,
    /// One subresource cache for all render threads, keeping scripts,
    /// styles and fonts for the whole crawl unless they say `no-store`, so a
    /// site's bundle is downloaded once, not once per page.
    pub cache_static: bool,
    /// Size of that cache.
    pub cache_mb: usize,
    /// GET fetch()/XHR responses are reused for this long across pages.
    pub fetch_cache_ttl: Option<Duration>,
    /// Layout time a page may spend; past it, layout reads get its last
    /// layout. Animation set-up that reads geometry after each write lays a
    /// big page out a hundred times otherwise. `None`: no limit.
    pub layout_budget: Option<Duration>,
    /// IntersectionObserver sees every target in the viewport, as with the
    /// very tall viewport search engines render with: lazy content loads, and
    /// no layout is computed to measure targets.
    pub intersection_full_viewport: bool,
    /// Layout downloads no images or web fonts: only pixels need them.
    pub skip_images: bool,
    pub skip_fonts: bool,
    /// IANA zone pages see (`Date`, `Intl`); the host's by default. A site
    /// that renders times on its server and again in the page, in another
    /// zone, logs hydration mismatches and re-renders.
    pub timezone: Option<String>,
    /// `navigator.language` and the Accept-Language of the page's requests,
    /// such as "ru-RU"; obscura's en-US by default.
    pub locale: Option<String>,
    /// Render the document the crawler already downloaded instead of
    /// requesting it again: one request less per rendered page. That
    /// document came with the crawler's user agent, not the renderer's.
    pub reuse_documents: bool,
}

/// A document the crawler already has, for `RenderPool::render_document`.
pub struct Document {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            // Rendering waits on the network as much as it computes: one
            // thread per core, within reason for memory: a thread holds
            // ~25 MB on simple pages and 300-400 MB at its peak on big
            // documentation pages (MDN, react.dev).
            threads: std::thread::available_parallelism().map_or(4, |cores| cores.get()).clamp(2, 16),
            // Settling on content quiet alone (SettlePolicy::content_quiet)
            // stays off: a spinner running while the data loads from
            // another site looks the same, and one sports.ru render under
            // load was taken there with 157 words of 9400.
            settle: SettlePolicy::default(),
            navigation_timeout: Duration::from_secs(30),
            user_agent: None,
            recycle_after: 200,
            allow_private_network: false,
            third_party_timeout: Some(Duration::from_secs(3)),
            block_trackers: true,
            cache_static: true,
            cache_mb: 256,
            fetch_cache_ttl: Some(Duration::from_secs(120)),
            layout_budget: Some(Duration::from_secs(2)),
            intersection_full_viewport: true,
            skip_images: true,
            skip_fonts: true,
            timezone: None,
            locale: None,
            reuse_documents: false,
        }
    }
}

/// One subresource or script-initiated request the render made.
#[derive(Debug, Clone, Serialize)]
pub struct Resource {
    pub url: String,
    /// obscura's resource type: Document, Script, Stylesheet, Image, Fetch...
    pub kind: String,
    /// 0 when the request failed or was blocked.
    pub status: u16,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rendered {
    pub url: String,
    /// The document URL after JavaScript redirects.
    pub final_url: String,
    pub error: Option<String>,
    /// Status of the main document response as the renderer saw it.
    pub status: Option<u16>,
    /// Serialised DOM after the content settled.
    pub html: String,
    pub dcl_ms: u64,
    pub total_ms: u64,
    /// The content went quiet before the budget ran out.
    pub settled: bool,
    pub load_fired: bool,
    pub resources: Vec<Resource>,
    /// Console messages of level error, including uncaught exceptions.
    pub console_errors: Vec<String>,
}

struct Job {
    url: String,
    document: Option<Document>,
    reply: oneshot::Sender<Rendered>,
}

/// Answers a render's request for its own document from what the crawler
/// downloaded, once; everything else goes to the network.
struct Prefetched(Arc<std::sync::Mutex<std::collections::HashMap<String, obscura_net::Response>>>);

#[async_trait::async_trait]
impl obscura_net::interceptor::RequestInterceptor for Prefetched {
    async fn intercept(&self, request: &obscura_net::RequestInfo) -> obscura_net::interceptor::InterceptAction {
        if request.method == "GET" && request.resource_type == obscura_net::ResourceType::Document {
            let prefetched = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(request.url.as_str());
            if let Some(response) = prefetched {
                return obscura_net::interceptor::InterceptAction::Fulfill(response);
            }
        }
        obscura_net::interceptor::InterceptAction::Continue
    }

    fn documents_only(&self) -> bool {
        true
    }
}

/// A fixed set of render threads sharing one job queue.
pub struct RenderPool {
    jobs: async_channel::Sender<Job>,
}

impl RenderPool {
    pub fn new(config: RenderConfig) -> Self {
        // The crawler reads the DOM, not pixels: skip obscura's image and font
        // preloading unless the environment asks for it.
        for key in [
            "OBSCURA_RENDER_RESOURCE_WARMUP_MS",
            "OBSCURA_RENDER_RESOURCE_POST_SCRIPT_WARMUP_MS",
            "OBSCURA_RENDER_RESOURCE_SETTLE_WARMUP_MS",
        ] {
            if std::env::var_os(key).is_none() {
                std::env::set_var(key, "0");
            }
        }
        // obscura reads its network switches from the environment; an
        // explicit setting there wins over the pool's defaults.
        let defaults = [
            ("OBSCURA_THIRD_PARTY_TIMEOUT_MS", config.third_party_timeout.map(|limit| limit.as_millis().to_string())),
            ("OBSCURA_BLOCK_TRACKERS", config.block_trackers.then(|| "1".to_string())),
            ("OBSCURA_RESOURCE_CACHE_SHARED", config.cache_static.then(|| "1".to_string())),
            // A crawl is a snapshot: static files stay valid for its length.
            ("OBSCURA_RESOURCE_CACHE_MIN_TTL_MS", config.cache_static.then(|| (6 * 3600 * 1000).to_string())),
            ("OBSCURA_RESOURCE_CACHE_MB", config.cache_static.then(|| config.cache_mb.to_string())),
            ("OBSCURA_FETCH_CACHE_TTL_MS", config.fetch_cache_ttl.map(|ttl| ttl.as_millis().to_string())),
            ("OBSCURA_LAYOUT_BUDGET_MS", config.layout_budget.map(|budget| budget.as_millis().to_string())),
            // V8 reads the zone from TZ when its first isolate starts.
            ("TZ", config.timezone.clone()),
            ("OBSCURA_INTERSECTION_FULL_VIEWPORT", config.intersection_full_viewport.then(|| "1".to_string())),
            ("OBSCURA_RENDER_SKIP_IMAGES", config.skip_images.then(|| "1".to_string())),
            ("OBSCURA_RENDER_SKIP_FONTS", config.skip_fonts.then(|| "1".to_string())),
        ];
        for (key, value) in defaults {
            if let (None, Some(value)) = (std::env::var_os(key), value) {
                std::env::set_var(key, value);
            }
        }
        if config.allow_private_network {
            // Script fetch() reads the switch from the environment; page
            // navigations take it from the context below.
            std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
        }
        let (tx, rx) = async_channel::unbounded();
        for worker in 0..config.threads.max(1) {
            spawn_worker(worker, rx.clone(), config.clone());
        }
        RenderPool { jobs: tx }
    }

    /// Renders `url` on the next free worker.
    pub async fn render(&self, url: String) -> Rendered {
        self.render_document(url, None).await
    }

    /// `render`, with the document already downloaded when the pool reuses
    /// documents (`RenderConfig::reuse_documents`).
    pub async fn render_document(&self, url: String, document: Option<Document>) -> Rendered {
        let (reply, rx) = oneshot::channel();
        if self.jobs.send(Job { url: url.clone(), document, reply }).await.is_err() {
            return Rendered::failed(url, "render pool is closed".into(), Instant::now());
        }
        rx.await
            .unwrap_or_else(|_| Rendered::failed(url, "render worker stopped".into(), Instant::now()))
    }
}

fn spawn_worker(worker: usize, jobs: async_channel::Receiver<Job>, config: RenderConfig) {
    std::thread::Builder::new()
        .name(format!("render-{worker}"))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(worker, %error, "render runtime failed to start");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async move {
                let context = Arc::new(BrowserContext::with_storage_and_network(
                    format!("render-{worker}"),
                    None,
                    false,
                    config.user_agent.clone(),
                    None,
                    config.allow_private_network,
                ));
                if let Some(locale) = &config.locale {
                    context.http_client.set_accept_language(&accept_language(locale)).await;
                }
                let prefetched = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
                if config.reuse_documents {
                    *context.http_client.interceptor.write().await = Some(Box::new(Prefetched(prefetched.clone())));
                }
                let new_page = |worker: usize, context: &Arc<BrowserContext>| {
                    let mut page = new_page(worker, context);
                    page.set_locale_override(config.locale.clone());
                    page
                };
                let mut page = new_page(worker, &context);
                let mut rendered_on_page = 0usize;
                while let Ok(job) = jobs.recv().await {
                    if rendered_on_page >= config.recycle_after {
                        page = new_page(worker, &context);
                        rendered_on_page = 0;
                    }
                    let document = job.document.filter(|_| config.reuse_documents).and_then(|document| {
                        let url = url::Url::parse(&job.url).ok()?;
                        Some(obscura_net::Response {
                            url,
                            status: document.status,
                            headers: document.headers.into_iter().map(|(name, value)| (name.to_ascii_lowercase(), value)).collect(),
                            body: document.body,
                            redirected_from: Vec::new(),
                        })
                    });
                    if let Some(document) = document {
                        prefetched.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(job.url.clone(), document);
                    }
                    let rendered = render(&mut page, &job.url, &config).await;
                    // Not asked for (the page's URL normalised differently):
                    // drop it rather than serve a later render.
                    prefetched.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&job.url);
                    rendered_on_page += 1;
                    if rendered.error.is_some() {
                        // A failed or abandoned navigation can leave the page
                        // mid-load; start the next job on a fresh one.
                        page = new_page(worker, &context);
                        rendered_on_page = 0;
                    }
                    let _ = job.reply.send(rendered);
                }
            });
        })
        .expect("spawn render thread");
}

/// "ru-RU" -> "ru-RU,ru;q=0.9", as a browser set to that language sends.
pub fn accept_language(locale: &str) -> String {
    match locale.split_once('-') {
        Some((language, _)) => format!("{locale},{language};q=0.9"),
        None => locale.to_string(),
    }
}

fn new_page(worker: usize, context: &Arc<BrowserContext>) -> Page {
    let page = Page::new(format!("render-{worker}"), context.clone());
    page.set_console_messages_enabled(true);
    page
}

async fn render(page: &mut Page, url: &str, config: &RenderConfig) -> Rendered {
    let started = Instant::now();
    tracing::debug!(url, "render start");
    let navigation = tokio::time::timeout(
        config.navigation_timeout,
        page.navigate_with_wait(url, WaitUntil::DomContentLoaded),
    )
    .await;
    let dcl_ms = started.elapsed().as_millis() as u64;
    match navigation {
        Err(_) => return Rendered::failed(url.to_string(), "navigation timed out".into(), started),
        Ok(Err(error)) => return Rendered::failed(url.to_string(), error.to_string(), started),
        Ok(Ok(())) => {}
    }
    tracing::debug!(url, dcl_ms, "render domcontentloaded");
    let settle = page.settle_content(config.settle).await;
    tracing::debug!(url, ms = started.elapsed().as_millis() as u64, settled = settle.settled, "render settled");
    let html = page
        .with_dom(|dom| match dom.query_selector("html") {
            Ok(Some(root)) => dom.outer_html(root),
            _ => dom.inner_html(dom.document()),
        })
        .unwrap_or_default();
    let resources = page
        .network_events
        .iter()
        .map(|event| Resource {
            url: event.url.clone(),
            kind: event.resource_type.clone(),
            status: event.status,
            error: event.error_text.clone(),
        })
        .collect::<Vec<_>>();
    let status = page
        .network_events
        .iter()
        .find(|event| event.resource_type == "Document")
        .map(|event| event.status)
        .filter(|status| *status != 0);
    let console_errors = page
        .take_pending_console_messages()
        .into_iter()
        .filter(|message| message.starts_with("[error]"))
        .map(|message| message.trim_start_matches("[error]").trim().to_string())
        .collect();
    Rendered {
        url: url.to_string(),
        final_url: page.url_string(),
        error: None,
        status,
        html,
        dcl_ms,
        total_ms: started.elapsed().as_millis() as u64,
        settled: settle.settled,
        load_fired: settle.load_fired,
        resources,
        console_errors,
    }
}

impl Rendered {
    fn failed(url: String, error: String, started: Instant) -> Self {
        let elapsed = started.elapsed().as_millis() as u64;
        Rendered {
            final_url: url.clone(),
            url,
            error: Some(error),
            status: None,
            html: String::new(),
            dcl_ms: elapsed,
            total_ms: elapsed,
            settled: false,
            load_fired: false,
            resources: Vec::new(),
            console_errors: Vec::new(),
        }
    }
}
