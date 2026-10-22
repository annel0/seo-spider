//! Crawl orchestration. One dispatcher owns the frontier; each URL becomes a
//! task that fetches it without following redirects (bounded by the HTTP
//! slots), renders HTML pages on the obscura pool, extracts facts from both
//! documents and prepares what the store needs. The dispatcher queues the
//! URLs an outcome points at and hands its writes to the store's own thread,
//! so redirect targets and links that only exist after JavaScript ran are
//! crawled like any other, and the disk never holds up the network.

mod audit;
mod config;
mod fetch;
mod robots;
mod writer;

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use regex::RegexSet;
use rustc_hash::{FxHashMap, FxHashSet};
use seo_page::{HttpInfo, PageFacts, Source};
use seo_render::{RenderPool, Rendered};
use seo_store::{encode_facts, BodyKind, RenderRecord, Store, Write};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;

pub use audit::run_checks;
pub use config::{CrawlConfig, FetchLevel, HttpConfig, RenderSettings, StoreOptions};
pub use seo_page::ExtractOptions;
pub use seo_render::{RenderConfig, SettlePolicy};

#[derive(Debug, Clone)]
pub struct CrawlSummary {
    pub crawl_id: i64,
    /// Every URL the crawl recorded, redirects and resources included.
    pub urls: usize,
    /// HTML documents read (whole or up to `</head>`).
    pub pages: usize,
    pub rendered: usize,
    pub render_errors: usize,
    /// Responses with status 4xx/5xx or no response at all.
    pub errors: usize,
    pub blocked_by_robots: usize,
    /// Bytes of response bodies actually read (a status check reads none).
    pub bytes: u64,
    pub elapsed: Duration,
    /// The crawl was asked to stop before its queue ran out.
    pub stopped: bool,
}

/// Where a running crawl is, as `CrawlControl::progress` reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CrawlProgress {
    /// URLs recorded so far.
    pub urls: usize,
    pub pages: usize,
    pub rendered: usize,
    pub errors: usize,
    /// URLs found and waiting for a request.
    pub queued: usize,
    pub in_flight: usize,
    pub bytes: u64,
    pub elapsed: Duration,
    pub stopping: bool,
}

/// What a caller holds on a running crawl: a switch that stops it and a
/// channel with its progress (latest value only, a few times a second).
#[derive(Default)]
pub struct CrawlControl {
    pub stop: Arc<AtomicBool>,
    pub progress: Option<tokio::sync::watch::Sender<CrawlProgress>>,
}

/// Crawls into `store` as `config` says, then hands the store back.
pub async fn crawl(config: CrawlConfig, store: Store) -> Result<(Store, CrawlSummary)> {
    crawl_with(config, store, CrawlControl::default()).await
}

/// `crawl`, ending early once `control.stop` is set: no new requests start,
/// the ones in flight finish and are stored, and the crawl is closed as
/// usual (depths, states) so its checks and exports work on what it got.
pub async fn crawl_with(
    mut config: CrawlConfig,
    store: Store,
    control: CrawlControl,
) -> Result<(Store, CrawlSummary)> {
    let CrawlControl { stop, progress } = control;
    let started = Instant::now();
    if config.start_urls.is_empty() {
        bail!("no start URL");
    }
    for change in config.resolve() {
        tracing::info!("setting turned on: {change}");
    }
    let options = fetch::ClientOptions {
        user_agent: &config.http.user_agent,
        timeout: config.http.timeout(),
        connect_timeout: config.http.connect_timeout(),
        concurrency: config.http.concurrency,
        headers: &config.http.headers,
        cookies: config.http.cookies,
        http2: config.http.http2,
        ca_certs: &config.http.ca_certs,
        accept_invalid_certs: config.http.accept_invalid_certs,
    };
    let client = fetch::client(&options)?;
    // The extra headers are for the site: its cookies or credentials must
    // not go to every host its pages link to.
    let no_headers = BTreeMap::new();
    let external_client = fetch::client(&fetch::ClientOptions { headers: &no_headers, ..options })?;
    let crawl_id = store.start_crawl(config.start_urls[0].as_str(), &serde_json::to_string(&config)?)?;
    let base_id = store.next_url_id()?;
    let writer = writer::Writer::spawn(store, crawl_id)?;

    let mut robots = robots::Robots::new(&config.http.robots_agent);
    for start in &config.start_urls {
        robots.load(&client, start).await;
    }

    let render = match &config.render {
        Some(settings) => {
            let mut render = settings.to_render_config();
            render.ca_certs = ca_bundle(&config.http.ca_certs)?;
            // The site's cookies go into the renderer's jar for the start
            // hosts (gazeta.ru and lenta.ru pass their Sber ID redirect on
            // one); other headers go with every request, so credentials stay
            // with the crawler: they would reach third-party hosts too.
            for (name, value) in &config.http.headers {
                if name.eq_ignore_ascii_case("cookie") {
                    for start in &config.start_urls {
                        for pair in value.split(';').map(str::trim).filter(|pair| !pair.is_empty()) {
                            render.cookies.push((start.to_string(), pair.to_string()));
                        }
                    }
                } else if !name.eq_ignore_ascii_case("authorization") && !name.eq_ignore_ascii_case("proxy-authorization") {
                    render.headers.push((name.clone(), value.clone()));
                }
            }
            Some(RenderPool::new(render))
        }
        None => None,
    };
    let render_threads = config.render.as_ref().map_or(0, |settings| settings.threads);
    let concurrency = config.http.concurrency.max(1);
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        external_client,
        render,
        reuse_documents: config.render.as_ref().is_some_and(|render| render.reuse_documents),
        http_slots: Semaphore::new(concurrency),
        external_slots: Semaphore::new(config.http.external_concurrency.max(1)),
        external_hosts: ExternalHosts::new(config.http.external_per_host),
        external_timeout: config.http.timeout().min(config.http.external_timeout()),
        pacer: {
            // The slower of max_rps and robots.txt's Crawl-delay.
            let from_rps = config.http.max_rps.filter(|rps| *rps > 0.0).map(|rps| Duration::from_secs_f64(1.0 / rps));
            let from_robots = config.respect_robots.then(|| robots.crawl_delay(&config.start_urls[0])).flatten();
            if let Some(delay) = from_robots {
                tracing::info!(delay_s = delay.as_secs_f64(), "robots.txt Crawl-delay");
            }
            from_rps.into_iter().chain(from_robots).max().map(Pacer::new)
        },
        head_for_status: config.http.head_for_status,
        retries: config.http.retries,
        body: match config.fetch {
            FetchLevel::Status => fetch::BodyMode::None,
            FetchLevel::Head => fetch::BodyMode::Head(config.http.head_max_bytes),
            FetchLevel::Full => fetch::BodyMode::Full(config.http.max_body),
        },
        raw_extract: match config.fetch {
            FetchLevel::Full => config.extract,
            _ => ExtractOptions::head_only(),
        },
        extract: config.extract,
        store: config.store.clone(),
        rules: Rules {
            scope: Scope::new(&config.start_urls, config.crawl_subdomains),
            filters: Filters::new(&config)?,
            follow_links: config.follow_links,
            follow_nofollow: config.follow_nofollow,
            check_external: config.check_external,
            check_resources: config.check_resources,
        },
    });
    let rules = &ctx.rules;
    // Tasks in flight per kind. Pages waiting for a render thread keep their
    // HTTP slot free, so allow a few of them per thread on top of the
    // requests; external checks have slots of their own and are capped
    // apart, so a backlog of them never crowds out the site's pages.
    let internal_cap = concurrency + render_threads * 2;
    // External tasks may wait on their host's slots; more of them than
    // slots keeps the slots busy with other hosts meanwhile.
    let external_cap = config.http.external_concurrency.max(1) * 4;
    let (mut internal_in_flight, mut external_in_flight) = (0usize, 0usize);

    let mut frontier = Frontier::new(config.max_urls, config.max_depth);
    for start in &config.start_urls {
        if let Some(url) = rules.filters.normalize(start.clone()) {
            let internal = rules.scope.internal(&url);
            frontier.push(url, 0, internal, false, None, Edge::Other);
        }
    }
    let roots: Vec<usize> = (0..frontier.known.len()).collect();

    let mut in_sitemap = HashSet::new();
    if config.use_sitemaps {
        let mut sitemaps = Vec::new();
        for start in &config.start_urls {
            let listed = robots.sitemaps(start);
            if listed.is_empty() {
                sitemaps.push(format!("{}/sitemap.xml", start.origin().ascii_serialization()));
            }
            sitemaps.extend(listed);
        }
        sitemaps.sort();
        sitemaps.dedup();
        in_sitemap = robots::sitemap_urls(&client, sitemaps, config.max_urls).await;
        tracing::info!(urls = in_sitemap.len(), "sitemaps read");
        for listed in &in_sitemap {
            let Some(url) = Url::parse(listed).ok().and_then(|url| rules.filters.normalize(url)) else { continue };
            if rules.scope.internal(&url) && rules.filters.allows(&url, true) {
                frontier.push(url, 1, true, false, None, Edge::Other);
            }
        }
    }

    let mut summary = CrawlSummary {
        crawl_id,
        urls: 0,
        pages: 0,
        rendered: 0,
        render_errors: 0,
        errors: 0,
        blocked_by_robots: 0,
        bytes: 0,
        elapsed: Duration::ZERO,
        stopped: false,
    };
    let mut tasks = JoinSet::new();
    // Which URL each task fetches, for a task that panics.
    let mut task_items: FxHashMap<tokio::task::Id, (usize, bool)> = FxHashMap::default();
    let mut last_progress = Instant::now();
    let mut last_report = Instant::now();
    // Where the dispatcher's time goes: it is the one task every outcome
    // passes through.
    let (mut waiting, mut storing, mut discovering) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let result: Result<()> = async {
        loop {
            loop {
                if stop.load(Ordering::Relaxed) {
                    if !summary.stopped {
                        summary.stopped = true;
                        tracing::info!(in_flight = tasks.len(), queued = frontier.queued(), "stopping");
                    }
                    break;
                }
                let item = if internal_in_flight < internal_cap {
                    frontier.next_internal()
                } else {
                    None
                };
                let item = match item {
                    Some(item) => item,
                    None if external_in_flight < external_cap => match frontier.next_external() {
                        Some(item) => item,
                        None => break,
                    },
                    None => break,
                };
                if item.internal {
                    robots.load(&client, &item.url).await;
                    if !robots.allowed(&item.url) {
                        frontier.known[item.index].blocked = true;
                        summary.blocked_by_robots += 1;
                        if config.respect_robots {
                            let http = HttpInfo {
                                url: item.url.to_string(),
                                error: Some("blocked by robots.txt".into()),
                                ..Default::default()
                            };
                            frontier.known[item.index].saved = true;
                            summary.urls += 1;
                            let id = base_id + item.index as i64;
                            writer.send(Write::Response { id, http, internal: true }).await?;
                            continue;
                        }
                    }
                }
                if item.internal {
                    internal_in_flight += 1;
                } else {
                    external_in_flight += 1;
                }
                let task_item = (item.index, item.internal);
                let task = tasks.spawn(process(ctx.clone(), item));
                task_items.insert(task.id(), task_item);
            }
            let waited = Instant::now();
            let Some(joined) = tasks.join_next_with_id().await else { break };
            waiting += waited.elapsed();
            let mut outcome = match joined {
                Ok((id, outcome)) => {
                    task_items.remove(&id);
                    outcome
                }
                Err(error) => {
                    // The URL gets a row saying so, and its slot is freed.
                    let Some((index, internal)) = task_items.remove(&error.id()) else { continue };
                    let url = frontier.known[index].url.clone();
                    tracing::error!(%url, %error, "crawl task failed");
                    if internal {
                        internal_in_flight -= 1;
                    } else {
                        external_in_flight -= 1;
                    }
                    let http = HttpInfo { url, error: Some(format!("crawler error: {error}")), ..Default::default() };
                    frontier.known[index].saved = true;
                    summary.urls += 1;
                    summary.errors += 1;
                    writer.send(Write::Response { id: base_id + index as i64, http, internal }).await?;
                    continue;
                }
            };
            if outcome.item.internal {
                internal_in_flight -= 1;
            } else {
                external_in_flight -= 1;
            }
            let discovered = Instant::now();
            for candidate in std::mem::take(&mut outcome.candidates) {
                frontier.push(
                    candidate.url,
                    candidate.depth,
                    candidate.internal,
                    candidate.resource,
                    Some(outcome.item.index),
                    candidate.edge,
                );
            }
            discovering += discovered.elapsed();
            let stored = Instant::now();
            record(&writer, base_id, outcome, config.store.links, &mut frontier, &mut summary).await?;
            storing += stored.elapsed();
            if let Some(progress) = progress.as_ref().filter(|_| last_report.elapsed() >= Duration::from_millis(250)) {
                last_report = Instant::now();
                progress.send_replace(CrawlProgress {
                    urls: summary.urls,
                    pages: summary.pages,
                    rendered: summary.rendered,
                    errors: summary.errors,
                    queued: frontier.queued(),
                    in_flight: tasks.len(),
                    bytes: summary.bytes,
                    elapsed: started.elapsed(),
                    stopping: summary.stopped,
                });
            }
            // The log line stands in for progress nobody else reports.
            if progress.is_none() && last_progress.elapsed() >= Duration::from_secs(2) {
                last_progress = Instant::now();
                tracing::info!(
                    done = summary.urls,
                    pages = summary.pages,
                    queued = frontier.queued(),
                    in_flight = tasks.len(),
                    "crawling"
                );
            }
        }
        Ok(())
    }
    .await;
    if let Some(progress) = &progress {
        progress.send_replace(CrawlProgress {
            urls: summary.urls,
            pages: summary.pages,
            rendered: summary.rendered,
            errors: summary.errors,
            queued: 0,
            in_flight: 0,
            bytes: summary.bytes,
            elapsed: started.elapsed(),
            stopping: summary.stopped,
        });
    }
    let loop_done = Instant::now();
    let finished = writer.finish();
    // A writer that failed is why sending stopped; report its error first.
    let mut store = finished.context("store writer")?;
    result?;
    tracing::info!(
        waiting_s = waiting.as_secs_f64(),
        storing_s = storing.as_secs_f64(),
        discovering_s = discovering.as_secs_f64(),
        "dispatcher time"
    );

    let depths = frontier.depths(&roots);
    let states: Vec<_> = frontier
        .known
        .iter()
        .zip(depths)
        .enumerate()
        .filter(|(_, (known, _))| known.saved)
        .map(|(index, (known, depth))| {
            (base_id + index as i64, depth, in_sitemap.contains(&known.url), known.blocked)
        })
        .collect();
    let states_done = Instant::now();
    store.set_crawl_states(&states)?;
    let saved = Instant::now();
    store.finish_crawl(crawl_id)?;
    tracing::info!(
        drain_s = (states_done - loop_done).as_secs_f64(),
        states_s = (saved - states_done).as_secs_f64(),
        indexes_s = saved.elapsed().as_secs_f64(),
        "finishing"
    );
    summary.elapsed = started.elapsed();
    Ok((store, summary))
}

struct Ctx {
    client: reqwest::Client,
    /// The same without the extra headers, for other hosts.
    external_client: reqwest::Client,
    render: Option<RenderPool>,
    /// Hand the renderer the document already downloaded.
    reuse_documents: bool,
    http_slots: Semaphore,
    external_slots: Semaphore,
    external_hosts: ExternalHosts,
    external_timeout: Duration,
    pacer: Option<Pacer>,
    head_for_status: bool,
    body: fetch::BodyMode,
    /// Extra attempts for a request that failed in a way worth retrying.
    retries: u32,
    /// What is read from the raw document: everything asked for when it is
    /// fetched whole, the `<head>` otherwise.
    raw_extract: ExtractOptions,
    /// What is read from the rendered DOM.
    extract: ExtractOptions,
    store: StoreOptions,
    rules: Rules,
}

/// What decides which URLs an outcome leads to.
struct Rules {
    scope: Scope,
    filters: Filters,
    follow_links: bool,
    follow_nofollow: bool,
    check_external: bool,
    check_resources: bool,
}

/// A URL an outcome points at, normalised and filtered on the worker.
struct Candidate {
    url: Url,
    depth: u32,
    internal: bool,
    resource: bool,
    edge: Edge,
}

/// How long to wait before trying a request again, if it is worth trying:
/// no response at all, 429, or (for the site) a 5xx. `Retry-After` is
/// honoured up to 30 s; otherwise the wait doubles from half a second.
fn retry_delay(http: &HttpInfo, internal: bool, attempt: u32) -> Option<Duration> {
    let retryable = match http.status {
        0 => http.error.as_deref().is_some_and(|error| !error.starts_with("blocked")),
        429 => true,
        500..=599 => internal,
        _ => false,
    };
    if !retryable {
        return None;
    }
    let backoff = Duration::from_millis(500 << attempt.min(5));
    let after = http
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(30)));
    Some(after.unwrap_or(backoff))
}

/// Per external host: a cap on its requests in flight, and whether it could
/// not be reached. Once a host fails to connect even on retry, the rest of
/// its URLs are reported with that error instead of each waiting it out.
struct ExternalHosts {
    per_host: usize,
    hosts: std::sync::Mutex<FxHashMap<String, ExternalHost>>,
}

struct ExternalHost {
    slots: Arc<Semaphore>,
    unreachable: Option<String>,
}

impl ExternalHosts {
    fn new(per_host: usize) -> Self {
        ExternalHosts { per_host: per_host.max(1), hosts: Default::default() }
    }

    fn with<T>(&self, host: &str, f: impl FnOnce(&mut ExternalHost) -> T) -> T {
        let mut hosts = self.hosts.lock().expect("external hosts lock");
        let entry = hosts.entry(host.to_string()).or_insert_with(|| ExternalHost {
            slots: Arc::new(Semaphore::new(self.per_host)),
            unreachable: None,
        });
        f(entry)
    }

    fn slots(&self, host: &str) -> Arc<Semaphore> {
        self.with(host, |entry| entry.slots.clone())
    }

    fn unreachable(&self, host: &str) -> Option<String> {
        self.with(host, |entry| entry.unreachable.clone())
    }

    fn mark_unreachable(&self, host: &str, error: &str) {
        self.with(host, |entry| {
            entry.unreachable.get_or_insert_with(|| error.to_string());
        });
    }
}

/// Spaces requests to the site an interval apart: max_rps or robots.txt Crawl-delay.
struct Pacer {
    interval: Duration,
    next: std::sync::Mutex<tokio::time::Instant>,
}

impl Pacer {
    fn new(interval: Duration) -> Self {
        Pacer { interval, next: std::sync::Mutex::new(tokio::time::Instant::now()) }
    }

    async fn pace(&self) {
        let at = {
            let mut next = self.next.lock().expect("pacer lock");
            let at = (*next).max(tokio::time::Instant::now());
            *next = at + self.interval;
            at
        };
        tokio::time::sleep_until(at).await;
    }
}

struct Item {
    url: Url,
    index: usize,
    depth: u32,
    /// Internal URLs are read and their HTML followed; external ones are
    /// requested for the status only.
    internal: bool,
    /// Found as an image, script, stylesheet or media file (or a redirect
    /// from one): fetched after the pages waiting.
    resource: bool,
}

/// A document read, with what the store needs prepared.
struct Document {
    facts: PageFacts,
    data: Vec<u8>,
}

struct Outcome {
    item: Item,
    http: HttpInfo,
    /// Body bytes read from the network.
    bytes_read: u64,
    raw: Option<Document>,
    raw_body: Option<Packed>,
    rendered: Option<RenderedPage>,
    /// URLs to queue, prepared on the worker.
    candidates: Vec<Candidate>,
}

struct RenderedPage {
    record: RenderRecord,
    document: Option<Document>,
    body: Option<Packed>,
}

/// A body as stored.
struct Packed {
    encoding: &'static str,
    size: usize,
    data: Vec<u8>,
}

fn pack(bytes: Vec<u8>, level: i32) -> Packed {
    let size = bytes.len();
    if level > 0 {
        if let Ok(data) = zstd::bulk::compress(&bytes, level) {
            return Packed { encoding: "zstd", size, data };
        }
    }
    Packed { encoding: "identity", size, data: bytes }
}

fn document(facts: PageFacts, level: i32) -> Document {
    let data = encode_facts(&facts, level).unwrap_or_default();
    Document { facts, data }
}

async fn process(ctx: Arc<Ctx>, item: Item) -> Outcome {
    let (http, body) = if item.internal {
        let _slot = ctx.http_slots.acquire().await.expect("HTTP slots are never closed");
        let fetched = fetch_retrying(&ctx, &item, ctx.body, None).await;
        (fetched.http, fetched.body)
    } else {
        let host = item.url.origin().ascii_serialization();
        let skipped = |error: String| HttpInfo {
            url: item.url.to_string(),
            error: Some(format!("not requested: {host} did not connect earlier ({error})")),
            ..Default::default()
        };
        match ctx.external_hosts.unreachable(&host) {
            Some(error) => (skipped(error), None),
            None => {
                let host_slots = ctx.external_hosts.slots(&host);
                let _host_slot = host_slots.acquire().await.expect("host slots are never closed");
                let _slot = ctx.external_slots.acquire().await.expect("HTTP slots are never closed");
                // The host may have failed while this waited for a slot.
                match ctx.external_hosts.unreachable(&host) {
                    Some(error) => (skipped(error), None),
                    None => {
                        let fetched =
                            fetch_retrying(&ctx, &item, fetch::BodyMode::None, Some(ctx.external_timeout)).await;
                        if fetched.unreachable {
                            let error = fetched.http.error.as_deref().unwrap_or("no connection");
                            ctx.external_hosts.mark_unreachable(&host, error);
                        }
                        (fetched.http, fetched.body)
                    }
                }
            }
        }
    };
    // Everything below is CPU work, done here on a runtime worker so it runs
    // in parallel across pages instead of on the dispatcher.
    let mut raw_body = None;
    let bytes_read = body.as_ref().map_or(0, |body| body.bytes.len() as u64);
    let renderable = item.internal && http.is_html() && (200..300).contains(&http.status);
    // Only a whole document stands in for the renderer's own request. The
    // body is decoded already, so its transfer headers do not apply.
    let downloaded = (ctx.reuse_documents
        && ctx.render.is_some()
        && renderable
        && matches!(ctx.body, fetch::BodyMode::Full(_))
        && http.error.is_none())
    .then_some(body.as_ref())
    .flatten()
    .map(|body| seo_render::Document {
        status: http.status,
        headers: http
            .headers
            .iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("content-encoding") && !name.eq_ignore_ascii_case("content-length"))
            .cloned()
            .collect(),
        body: body.bytes.clone(),
    });
    let raw = body.map(|body| {
        let facts = seo_page::extract_with(&body.html, &item.url, Source::Raw, &ctx.raw_extract);
        if ctx.store.raw_html {
            raw_body = Some(pack(body.bytes, ctx.store.compression_level));
        }
        document(facts, ctx.store.compression_level)
    });
    let rendered = match &ctx.render {
        Some(pool) if renderable => Some(render_page(pool, &item.url, downloaded, &ctx).await),
        _ => None,
    };
    let mut outcome = Outcome { item, http, bytes_read, raw, raw_body, rendered, candidates: Vec::new() };
    outcome.candidates = discover(&outcome, &ctx.rules);
    outcome
}

/// The renderer takes one PEM file of extra roots: several are joined into
/// a temporary one.
fn ca_bundle(paths: &[std::path::PathBuf]) -> anyhow::Result<Option<std::path::PathBuf>> {
    match paths {
        [] => Ok(None),
        [path] => Ok(Some(path.clone())),
        paths => {
            let mut pem = Vec::new();
            for path in paths {
                pem.extend(std::fs::read(path)?);
                pem.push(b'\n');
            }
            let bundle = std::env::temp_dir().join(format!("seo-spider-ca-{}.pem", std::process::id()));
            std::fs::write(&bundle, pem)?;
            Ok(Some(bundle))
        }
    }
}

/// Requests the item, again while the failure is one worth retrying.
async fn fetch_retrying(ctx: &Ctx, item: &Item, mode: fetch::BodyMode, timeout: Option<Duration>) -> fetch::Fetched {
    // External URLs are only retried after a network failure: a third
    // party answering 5xx is the answer worth reporting.
    let retries = if item.internal { ctx.retries } else { ctx.retries.min(1) };
    let mut attempt = 0;
    loop {
        if item.internal {
            if let Some(pacer) = &ctx.pacer {
                pacer.pace().await;
            }
        }
        let client = if item.internal { &ctx.client } else { &ctx.external_client };
        let fetched = fetch::fetch(client, &item.url, mode, timeout, ctx.head_for_status).await;
        match retry_delay(&fetched.http, item.internal, attempt) {
            Some(delay) if attempt < retries => {
                tracing::debug!(url = %item.url, attempt, status = fetched.http.status, "retrying");
                attempt += 1;
                tokio::time::sleep(delay).await;
            }
            _ => return fetched,
        }
    }
}

async fn render_page(
    pool: &RenderPool,
    url: &Url,
    mut downloaded: Option<seo_render::Document>,
    ctx: &Ctx,
) -> RenderedPage {
    let mut attempt = 0;
    let rendered = loop {
        let rendered = pool.render_document(url.to_string(), downloaded.take()).await;
        // The renderer's own request for the document can be reset or
        // refused where the crawler's went through: retry it as the
        // crawler retries a request that got no answer.
        let unanswered = rendered.error.as_deref().is_some_and(|error| error.starts_with("Network error"));
        if !unanswered || attempt >= ctx.retries {
            break rendered;
        }
        tracing::debug!(url = %url, attempt, "retrying render");
        tokio::time::sleep(Duration::from_millis(500 << attempt.min(5))).await;
        attempt += 1;
        if let Some(pacer) = &ctx.pacer {
            pacer.pace().await;
        }
    };
    let Rendered { final_url, error, html, dcl_ms, total_ms, settled, load_fired, resources, console_errors, .. } =
        rendered;
    let (document, body) = if error.is_none() {
        let base = Url::parse(&final_url).unwrap_or_else(|_| url.clone());
        let facts = seo_page::extract_with(&html, &base, Source::Rendered, &ctx.extract);
        let body = ctx.store.rendered_html.then(|| pack(html.into_bytes(), ctx.store.compression_level));
        (Some(document(facts, ctx.store.compression_level)), body)
    } else {
        (None, None)
    };
    RenderedPage {
        record: RenderRecord {
            final_url,
            error,
            dcl_ms,
            total_ms,
            settled,
            load_fired,
            console_errors_json: serde_json::to_string(&console_errors).unwrap_or_default(),
            resources_json: serde_json::to_string(&resources).unwrap_or_default(),
        },
        document,
        body,
    }
}

async fn record(
    writer: &writer::Writer,
    base_id: i64,
    outcome: Outcome,
    links: bool,
    frontier: &mut Frontier,
    summary: &mut CrawlSummary,
) -> Result<()> {
    let Outcome { item, http, bytes_read, raw, raw_body, rendered, .. } = outcome;
    let url_id = base_id + item.index as i64;
    frontier.known[item.index].saved = true;
    summary.urls += 1;
    if http.status == 0 || http.status >= 400 {
        summary.errors += 1;
    }
    summary.bytes += bytes_read;
    writer.send(Write::Response { id: url_id, http, internal: item.internal }).await?;
    if let Some(Document { facts, data }) = raw {
        summary.pages += 1;
        writer.send(Write::Facts { url_id, facts: Box::new(facts), data, links }).await?;
    }
    if let Some(Packed { encoding, size, data }) = raw_body {
        writer.send(Write::Body { url_id, kind: BodyKind::Raw, encoding, size, data }).await?;
    }
    if let Some(RenderedPage { record, document, body }) = rendered {
        summary.rendered += 1;
        if record.error.is_some() {
            summary.render_errors += 1;
        }
        writer.send(Write::Render { url_id, render: Box::new(record) }).await?;
        if let Some(Document { facts, data }) = document {
            writer.send(Write::Facts { url_id, facts: Box::new(facts), data, links }).await?;
        }
        if let Some(Packed { encoding, size, data }) = body {
            writer.send(Write::Body { url_id, kind: BodyKind::Rendered, encoding, size, data }).await?;
        }
    }
    Ok(())
}

/// What the outcome points at: the redirect target, then the URLs of the
/// raw and rendered documents the settings ask to follow or check, each
/// (URL, edge) once.
fn discover(outcome: &Outcome, rules: &Rules) -> Vec<Candidate> {
    let depth = outcome.item.depth;
    let from_resource = outcome.item.resource;
    let mut seen = FxHashSet::default();
    let mut candidates = Vec::new();
    let mut queue = |href: &str, depth: u32, edge: Edge| {
        if !seen.insert((href.to_string(), edge)) {
            return;
        }
        let Some(url) = Url::parse(href).ok().and_then(|url| rules.filters.normalize(url)) else { return };
        let internal = rules.scope.internal(&url);
        match edge {
            Edge::Resource if !rules.check_resources => return,
            Edge::Click | Edge::Other if !internal && !rules.check_external => return,
            Edge::Click | Edge::Other if internal && !rules.follow_links => return,
            _ => {}
        }
        if !rules.filters.allows(&url, internal) {
            return;
        }
        let resource = edge == Edge::Resource || (edge == Edge::Redirect && from_resource);
        candidates.push(Candidate { url, depth, internal, resource, edge });
    };
    if outcome.http.is_redirect() {
        if let Some(location) = &outcome.http.location {
            queue(location, depth, Edge::Redirect);
        }
    }
    // Only internal documents are read, so their links are followed.
    let documents = [
        outcome.raw.as_ref().map(|document| &document.facts),
        outcome.rendered.as_ref().and_then(|page| page.document.as_ref()).map(|document| &document.facts),
    ];
    for facts in documents.into_iter().flatten() {
        let next = depth + 1;
        for link in &facts.links {
            if link.href.is_empty() || (link.nofollow && !rules.follow_nofollow) {
                continue;
            }
            queue(&link.href, next, Edge::Click);
        }
        if let Some((_, Some(target))) = &facts.meta_refresh {
            queue(target, depth, Edge::Redirect);
        }
        let alternates = facts
            .canonicals
            .iter()
            .chain(facts.hreflangs.iter().map(|hreflang| &hreflang.href))
            .chain(facts.pagination.iter().map(|page| &page.href))
            .chain(&facts.iframes);
        for href in alternates.filter(|href| !href.is_empty()) {
            queue(href, next, Edge::Other);
        }
        let resources = facts
            .images
            .iter()
            .map(|image| &image.src)
            .chain(&facts.scripts)
            .chain(&facts.stylesheets)
            .chain(&facts.audios)
            .chain(&facts.videos);
        for href in resources {
            queue(href, next, Edge::Resource);
        }
    }
    candidates
}

/// Include/exclude patterns and query parameters to drop.
struct Filters {
    include: RegexSet,
    exclude: RegexSet,
    strip: Vec<String>,
}

impl Filters {
    fn new(config: &CrawlConfig) -> Result<Self> {
        Ok(Filters {
            include: RegexSet::new(&config.include).context("include pattern")?,
            exclude: RegexSet::new(&config.exclude).context("exclude pattern")?,
            strip: config.strip_params.clone(),
        })
    }

    /// The URL as the crawl knows it: no fragment, no stripped parameters.
    fn normalize(&self, mut url: Url) -> Option<Url> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        url.set_fragment(None);
        if !self.strip.is_empty() && url.query().is_some() {
            let kept: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(name, _)| !self.strip.iter().any(|pattern| param_matches(pattern, name)))
                .map(|(name, value)| (name.into_owned(), value.into_owned()))
                .collect();
            if kept.is_empty() {
                url.set_query(None);
            } else {
                url.query_pairs_mut().clear().extend_pairs(kept);
            }
        }
        Some(url)
    }

    /// Whether a URL may be queued: not excluded, and for an internal URL
    /// included when inclusion patterns are given.
    fn allows(&self, url: &Url, internal: bool) -> bool {
        let text = url.as_str();
        if self.exclude.is_match(text) {
            return false;
        }
        !internal || self.include.is_empty() || self.include.is_match(text)
    }
}

fn param_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

/// Which hosts are internal.
struct Scope {
    hosts: HashSet<String>,
    /// The start hosts without `www.`, for subdomain matching.
    bases: Vec<String>,
    subdomains: bool,
}

impl Scope {
    fn new(starts: &[Url], subdomains: bool) -> Self {
        let hosts: HashSet<String> =
            starts.iter().filter_map(|url| url.host_str()).map(str::to_ascii_lowercase).collect();
        let mut bases: Vec<String> =
            hosts.iter().map(|host| host.strip_prefix("www.").unwrap_or(host).to_string()).collect();
        bases.sort();
        bases.dedup();
        Scope { hosts, bases, subdomains }
    }

    /// A start host in either scheme, or a subdomain when the crawl covers them.
    fn internal(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else { return false };
        self.hosts.contains(host)
            || (self.subdomains
                && self.bases.iter().any(|base| {
                    host == base || host.strip_suffix(base.as_str()).is_some_and(|rest| rest.ends_with('.'))
                }))
    }
}

/// How a URL was found from another, for click depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Edge {
    /// A followed link: one click deeper.
    Click,
    /// An HTTP redirect or meta refresh: the same click.
    Redirect,
    /// An image, script, stylesheet or media file of the page.
    Resource,
    /// Canonical, hreflang, pagination, iframe, sitemap: no click.
    Other,
}

struct Known {
    url: String,
    /// The crawl wrote a row for it.
    saved: bool,
    blocked: bool,
    /// (target, edge) for every click, redirect and resource from this URL.
    edges: Vec<(u32, Edge)>,
}

/// URLs known to the crawl in discovery order and the ones still to fetch.
/// A URL's position here is also its id offset in the store.
struct Frontier {
    pages: VecDeque<Item>,
    resources: VecDeque<Item>,
    external: VecDeque<Item>,
    index: FxHashMap<String, usize>,
    known: Vec<Known>,
    /// URLs taken from each budget: internal pages, internal resources,
    /// external URLs; each may reach `max_urls`.
    counts: [usize; 3],
    max_urls: usize,
    max_depth: Option<u32>,
}

impl Frontier {
    fn new(max_urls: usize, max_depth: Option<u32>) -> Self {
        Frontier {
            pages: VecDeque::new(),
            resources: VecDeque::new(),
            external: VecDeque::new(),
            index: FxHashMap::default(),
            known: Vec::new(),
            counts: [0; 3],
            max_urls,
            max_depth,
        }
    }

    fn push(&mut self, url: Url, depth: u32, internal: bool, resource: bool, from: Option<usize>, edge: Edge) {
        let index = match self.index.get(url.as_str()) {
            Some(&index) => index,
            None => {
                let budget = match (internal, resource) {
                    (true, false) => 0,
                    (true, true) => 1,
                    (false, _) => 2,
                };
                if self.counts[budget] >= self.max_urls || self.max_depth.is_some_and(|max| depth > max) {
                    return;
                }
                self.counts[budget] += 1;
                let index = self.known.len();
                self.index.insert(url.to_string(), index);
                self.known.push(Known { url: url.to_string(), saved: false, blocked: false, edges: Vec::new() });
                let item = Item { url, index, depth, internal, resource };
                match budget {
                    0 => self.pages.push_back(item),
                    1 => self.resources.push_back(item),
                    _ => self.external.push_back(item),
                }
                index
            }
        };
        if let Some(from) = from.filter(|&from| from != index && edge != Edge::Other) {
            self.known[from].edges.push((index as u32, edge));
        }
    }

    /// The next internal URL: pages first, resources once no page waits.
    fn next_internal(&mut self) -> Option<Item> {
        self.pages.pop_front().or_else(|| self.resources.pop_front())
    }

    fn next_external(&mut self) -> Option<Item> {
        self.external.pop_front()
    }

    fn queued(&self) -> usize {
        self.pages.len() + self.resources.len() + self.external.len()
    }

    /// Clicks from the nearest root to each known URL: links cost one,
    /// redirects none, and a resource sits one below its page. `None` for
    /// URLs nothing reaches, such as pages found only in a sitemap.
    fn depths(&self, roots: &[usize]) -> Vec<Option<u32>> {
        let mut depths: Vec<Option<u32>> = vec![None; self.known.len()];
        // 0-1 breadth-first search: redirects go to the front of the queue.
        let mut queue = VecDeque::new();
        for &root in roots {
            depths[root] = Some(0);
            queue.push_back(root);
        }
        while let Some(at) = queue.pop_front() {
            let here = depths[at].unwrap_or(0);
            for &(to, edge) in &self.known[at].edges {
                let to = to as usize;
                let cost = match edge {
                    Edge::Click => 1,
                    Edge::Redirect => 0,
                    Edge::Resource | Edge::Other => continue,
                };
                if depths[to].is_none_or(|depth| here + cost < depth) {
                    depths[to] = Some(here + cost);
                    if cost == 0 {
                        queue.push_front(to);
                    } else {
                        queue.push_back(to);
                    }
                }
            }
        }
        let mut resources = depths.clone();
        for (at, known) in self.known.iter().enumerate() {
            let Some(here) = depths[at] else { continue };
            for &(to, edge) in &known.edges {
                let to = to as usize;
                if edge == Edge::Resource && resources[to].is_none_or(|depth| here + 1 < depth) {
                    resources[to] = Some(here + 1);
                }
            }
        }
        resources
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn scope_matches_hosts() {
        let scope = Scope::new(&[url("https://www.example.com/")], false);
        assert!(scope.internal(&url("http://www.example.com/a")));
        assert!(!scope.internal(&url("https://blog.example.com/")));
        let scope = Scope::new(&[url("https://www.example.com/")], true);
        assert!(scope.internal(&url("https://blog.example.com/")));
        assert!(scope.internal(&url("https://example.com/")));
        assert!(!scope.internal(&url("https://notexample.com/")));
        let scope = Scope::new(&[url("https://a.com/"), url("https://b.com/x")], false);
        assert!(scope.internal(&url("https://b.com/y")));
    }

    #[test]
    fn depths_follow_clicks_only() {
        let mut frontier = Frontier::new(100, None);
        frontier.push(url("https://e.com/"), 0, true, false, None, Edge::Other);
        frontier.push(url("https://e.com/a"), 1, true, false, Some(0), Edge::Click);
        frontier.push(url("https://e.com/b"), 2, true, false, Some(1), Edge::Click);
        frontier.push(url("https://e.com/b"), 1, true, false, Some(0), Edge::Click);
        frontier.push(url("https://e.com/canonical"), 1, true, false, Some(0), Edge::Other);
        frontier.push(url("https://e.com/orphan"), 1, true, false, None, Edge::Other);
        frontier.push(url("https://e.com/moved"), 2, true, false, Some(1), Edge::Redirect);
        frontier.push(url("https://e.com/img.png"), 2, true, true, Some(1), Edge::Resource);
        assert_eq!(frontier.known.len(), 7);
        assert_eq!(
            frontier.depths(&[0]),
            vec![Some(0), Some(1), Some(1), None, None, Some(1), Some(2)]
        );
    }

    #[test]
    fn limits_stop_queueing() {
        let mut frontier = Frontier::new(2, Some(1));
        frontier.push(url("https://e.com/"), 0, true, false, None, Edge::Other);
        frontier.push(url("https://e.com/deep"), 2, true, false, Some(0), Edge::Click);
        frontier.push(url("https://e.com/a"), 1, true, false, Some(0), Edge::Click);
        frontier.push(url("https://e.com/b"), 1, true, false, Some(0), Edge::Click);
        frontier.push(url("https://other.com/"), 1, false, false, Some(0), Edge::Click);
        // Resources have a budget of their own and wait behind pages.
        frontier.push(url("https://e.com/a.png"), 1, true, true, Some(0), Edge::Resource);
        assert_eq!(frontier.known.len(), 4);
        assert_eq!(frontier.next_internal().unwrap().url.path(), "/");
        assert_eq!(frontier.next_internal().unwrap().url.path(), "/a");
        assert_eq!(frontier.next_internal().unwrap().url.path(), "/a.png");
        assert_eq!(frontier.next_external().unwrap().url.host_str(), Some("other.com"));
    }

    #[test]
    fn retries_what_may_succeed_next_time() {
        let http = |status: u16, error: Option<&str>, headers: Vec<(&str, &str)>| HttpInfo {
            status,
            error: error.map(str::to_string),
            headers: headers.into_iter().map(|(name, value)| (name.to_string(), value.to_string())).collect(),
            ..Default::default()
        };
        assert_eq!(retry_delay(&http(0, Some("timeout"), vec![]), true, 0), Some(Duration::from_millis(500)));
        assert_eq!(retry_delay(&http(0, Some("timeout"), vec![]), true, 1), Some(Duration::from_millis(1000)));
        assert_eq!(retry_delay(&http(503, None, vec![]), true, 0), Some(Duration::from_millis(500)));
        assert_eq!(retry_delay(&http(503, None, vec![]), false, 0), None);
        assert_eq!(retry_delay(&http(429, None, vec![("retry-after", "3")]), false, 0), Some(Duration::from_secs(3)));
        assert_eq!(retry_delay(&http(429, None, vec![("retry-after", "600")]), true, 0), Some(Duration::from_secs(30)));
        assert_eq!(retry_delay(&http(404, None, vec![]), true, 0), None);
        assert_eq!(retry_delay(&http(200, None, vec![]), true, 0), None);
    }

    #[test]
    fn filters_normalize_and_select() {
        let mut config = CrawlConfig::new(url("https://e.com/"));
        config.include = vec!["/blog/".into()];
        config.exclude = vec![r"\?page=".into()];
        config.strip_params = vec!["utm_*".into(), "ref".into()];
        let filters = Filters::new(&config).unwrap();
        let normalized = filters.normalize(url("https://e.com/blog/a?utm_source=x&id=2&ref=3#top")).unwrap();
        assert_eq!(normalized.as_str(), "https://e.com/blog/a?id=2");
        assert_eq!(filters.normalize(url("https://e.com/?utm_medium=y")).unwrap().as_str(), "https://e.com/");
        assert!(filters.normalize(url("mailto:a@e.com")).is_none());
        assert!(filters.allows(&url("https://e.com/blog/a"), true));
        assert!(!filters.allows(&url("https://e.com/shop/a"), true));
        assert!(filters.allows(&url("https://other.com/shop"), false));
        assert!(!filters.allows(&url("https://e.com/blog/?page=2"), true));
        // Cloudflare's email placeholder is skipped unless a config drops it.
        let defaults = Filters::new(&CrawlConfig::default()).unwrap();
        assert!(!defaults.allows(&url("https://e.com/cdn-cgi/l/email-protection"), true));
        assert!(defaults.allows(&url("https://e.com/contact"), true));
    }
}
