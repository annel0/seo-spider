//! Crawl orchestration. One dispatcher owns the frontier; each URL becomes a
//! task that fetches it without following redirects (bounded by the HTTP
//! slots), renders HTML pages on the obscura pool, extracts facts from both
//! documents and prepares what the store needs. The dispatcher queues the
//! URLs an outcome points at and hands its writes to the store's own thread,
//! so redirect targets and links that only exist after JavaScript ran are
//! crawled like any other, and the disk never holds up the network.

mod audit;
mod auto;
mod config;
mod fetch;
mod resume;
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
pub use auto::{RenderDecision, Sample};
pub use config::{is_private_host, CrawlConfig, FetchLevel, HttpConfig, RenderSettings, StoreOptions};
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
    /// The crawl's id in the store, sent as soon as it is recorded.
    pub crawl_id: i64,
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
    /// Paused: no request starts until `pause` is cleared.
    pub paused: bool,
    /// The auto mode is reading pages both ways before the crawl.
    pub probing: bool,
}

/// What a caller holds on a running crawl: switches that stop and pause it,
/// a channel with its progress (latest value only, a few times a second),
/// and whether each page's issues are found and stored as it is crawled
/// (for a live view; the checks after the crawl replace them).
#[derive(Default)]
pub struct CrawlControl {
    pub stop: Arc<AtomicBool>,
    /// While set, no request starts; those in flight finish.
    pub pause: Arc<AtomicBool>,
    pub progress: Option<tokio::sync::watch::Sender<CrawlProgress>>,
    pub live_issues: bool,
}

/// How often a crawl with a progress channel reports.
const REPORT_EVERY: Duration = Duration::from_millis(250);

fn progress_of(summary: &CrawlSummary, queued: usize, in_flight: usize, started: Instant, paused: bool) -> CrawlProgress {
    CrawlProgress {
        crawl_id: summary.crawl_id,
        urls: summary.urls,
        pages: summary.pages,
        rendered: summary.rendered,
        errors: summary.errors,
        queued,
        in_flight,
        bytes: summary.bytes,
        elapsed: started.elapsed(),
        stopping: summary.stopped,
        paused,
        probing: false,
    }
}

/// Crawls into `store` as `config` says, then hands the store back.
pub async fn crawl(config: CrawlConfig, store: Store) -> Result<(Store, CrawlSummary)> {
    crawl_with(config, store, CrawlControl::default()).await
}

/// `crawl`, ending early once `control.stop` is set: no new requests start,
/// the ones in flight finish and are stored, and the crawl is closed as
/// usual (depths, states) so its checks and exports work on what it got.
pub async fn crawl_with(config: CrawlConfig, store: Store, control: CrawlControl) -> Result<(Store, CrawlSummary)> {
    run(config, store, control, None).await
}

/// Goes on with crawl `crawl_id` of `store`, interrupted or stopped, under
/// `config`: its settings as `Store::crawl_config` holds them, perhaps with
/// a higher `max_urls`. What it wrote stays and keeps its share of the
/// budgets; the links of its pages are found again from their stored facts
/// by the same rules, and what nothing fetched yet is queued (see
/// `resume`). Robots.txt and the sitemaps are read again, and the crawl is
/// closed as one: depths and states over all of its URLs.
pub async fn continue_with(
    config: CrawlConfig,
    store: Store,
    crawl_id: i64,
    control: CrawlControl,
) -> Result<(Store, CrawlSummary)> {
    run(config, store, control, Some(crawl_id)).await
}

async fn run(mut config: CrawlConfig, store: Store, control: CrawlControl, resume: Option<i64>) -> Result<(Store, CrawlSummary)> {
    let CrawlControl { stop, pause, progress, live_issues } = control;
    let started = Instant::now();
    if config.start_urls.is_empty() {
        bail!("no start URL");
    }
    for change in config.resolve() {
        tracing::info!("setting turned on: {change}");
    }
    // Checked before the crawl is recorded: a bad selector stops it at once.
    let custom = seo_page::CustomRules::compile(&config.custom.extract, &config.custom.search)
        .map_err(|error| anyhow::anyhow!("custom extraction: {error}"))?;
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
    let config_json = serde_json::to_string(&config)?;
    let crawl_id = match resume {
        Some(crawl_id) => {
            store.reopen_crawl(crawl_id, &config_json)?;
            crawl_id
        }
        None => store.start_crawl(config.start_urls[0].as_str(), &config_json)?,
    };
    if let Some(progress) = &progress {
        progress.send_replace(CrawlProgress { crawl_id, ..CrawlProgress::default() });
    }

    let mut robots = robots::Robots::new(&config.http.robots_agent);
    for start in &config.start_urls {
        robots.load(&client, start).await;
    }
    let rules = Rules {
        scope: Scope::new(&config.start_urls, config.crawl_subdomains),
        filters: Filters::new(&config)?,
        follow_links: config.follow_links,
        follow_nofollow: config.follow_nofollow,
        check_external: config.check_external,
        check_resources: config.check_resources,
    };
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

    // The render pool comes first: the auto mode reads pages with it.
    let mut render = match &config.render {
        Some(settings) => {
            let mut render = settings.to_render_config();
            render.ca_certs = ca_bundle(&config.http.ca_certs)?;
            render.accept_invalid_certs = config.http.accept_invalid_certs;
            (render.cookies, render.headers) = render_headers(&config.http.headers, &config.start_urls);
            Some(RenderPool::new(render))
        }
        None => None,
    };
    // The auto mode decides once per crawl, before it; a continued crawl
    // finds the decision in its settings.
    let auto_pages = config
        .render
        .as_ref()
        .filter(|settings| settings.auto && config.render_decision.is_none())
        .map(|settings| settings.auto_pages.max(1));
    if let (Some(pages), Some(pool)) = (auto_pages, render.as_ref()) {
        if let Some(progress) = &progress {
            progress.send_replace(CrawlProgress { crawl_id, probing: true, ..CrawlProgress::default() });
        }
        let probed = Instant::now();
        let decision = auto::probe(&client, pool, &rules, &robots, &config, pages).await;
        tracing::info!(render = decision.render, reason = %decision.reason, seconds = probed.elapsed().as_secs_f64(), "auto mode");
        if !decision.render {
            config.render = None;
        }
        config.render_decision = Some(decision);
        store.update_crawl_config(crawl_id, &serde_json::to_string(&config)?)?;
        if let Some(progress) = &progress {
            progress.send_replace(CrawlProgress { crawl_id, ..CrawlProgress::default() });
        }
    }
    if config.render.is_none() {
        render = None;
    }

    // A continued crawl's URLs are known first: they keep their ids, and
    // the budgets are what they left.
    let mut frontier = Frontier::new(config.max_urls, config.max_depth, store.next_url_id()?);
    let registered = match resume {
        Some(crawl_id) => Some(resume::register(&store, crawl_id, &mut frontier)?),
        None => None,
    };
    for start in &config.start_urls {
        if let Some(url) = rules.filters.normalize(start.clone()) {
            let internal = rules.scope.internal(&url);
            frontier.push(url, 0, internal, false, None, Edge::Other);
        }
    }
    let roots: Vec<usize> = config
        .start_urls
        .iter()
        .filter_map(|start| rules.filters.normalize(start.clone()))
        .filter_map(|url| frontier.index.get(url.as_str()).copied())
        .collect();

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

    if let (Some(crawl_id), Some(registered)) = (resume, &registered) {
        let replayed = Instant::now();
        let reads_documents = config.fetch != FetchLevel::Status;
        let again = resume::replay(&store, crawl_id, &mut frontier, registered, &rules, reads_documents, &mut summary)?;
        store.delete_urls(&again)?;
        // An interrupted crawl never wrote which of its URLs robots.txt
        // disallows; robots.txt is read again, so it says it again.
        for &index in &registered.internal {
            let Ok(url) = Url::parse(&frontier.known[index].url) else { continue };
            robots.load(&client, &url).await;
            frontier.known[index].blocked |= !robots.allowed(&url);
        }
        summary.blocked_by_robots = frontier.known.iter().filter(|known| known.saved && known.blocked).count();
        tracing::info!(
            written = summary.urls,
            queued = frontier.queued(),
            seconds = replayed.elapsed().as_secs_f64(),
            "continuing"
        );
    }
    let writer = writer::Writer::spawn(store, crawl_id)?;

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
            let floor = from_rps.into_iter().chain(from_robots).max();
            (floor.is_some() || config.http.auto_throttle)
                .then(|| Pacer::new(floor.unwrap_or_default(), config.http.auto_throttle))
        },
        head_for_status: config.http.head_for_status,
        stop: stop.clone(),
        live_issues,
        in_sitemap: std::sync::OnceLock::new(),
        retries: config.http.retries,
        body: match config.fetch {
            FetchLevel::Status => fetch::BodyMode::None,
            FetchLevel::Head => {
                fetch::BodyMode::Head { limit: config.http.head_max_bytes, drain: config.http.head_drain_bytes }
            }
            FetchLevel::Full => fetch::BodyMode::Full(config.http.max_body),
        },
        raw_extract: match config.fetch {
            FetchLevel::Full => config.extract,
            _ => ExtractOptions::head_only(),
        },
        extract: config.extract,
        custom,
        store: config.store.clone(),
        rules,
    });
    if live_issues && config.use_sitemaps {
        let _ = ctx.in_sitemap.set(in_sitemap.clone());
    }
    // Tasks in flight per kind. Pages waiting for a render thread keep their
    // HTTP slot free, so allow a few of them per thread on top of the
    // requests; external checks have slots of their own and are capped
    // apart, so a backlog of them never crowds out the site's pages.
    let internal_cap = concurrency + render_threads * 2;
    // External tasks may wait on their host's slots; more of them than
    // slots keeps the slots busy with other hosts meanwhile.
    let external_cap = config.http.external_concurrency.max(1) * 4;
    let (mut internal_in_flight, mut external_in_flight) = (0usize, 0usize);

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
                if pause.load(Ordering::Relaxed) {
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
                            let id = frontier.known[item.index].id;
                            writer.send(Write::Response { id, http, internal: true, depth: Some(item.depth) }).await?;
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
            // Paused with nothing in flight: wait for resume or stop instead
            // of taking the empty task set for the end of the crawl.
            if tasks.is_empty() && pause.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                if let Some(progress) = &progress {
                    progress.send_replace(progress_of(&summary, frontier.queued(), 0, started, true));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let waited = Instant::now();
            // With a progress channel, wait at most a report interval: a crawl
            // waiting on a slow request still reports.
            let joined = match &progress {
                Some(progress) => match tokio::time::timeout(REPORT_EVERY, tasks.join_next_with_id()).await {
                    Ok(joined) => joined,
                    Err(_) => {
                        waiting += waited.elapsed();
                        last_report = Instant::now();
                        let paused = pause.load(Ordering::Relaxed);
                        progress.send_replace(progress_of(&summary, frontier.queued(), tasks.len(), started, paused));
                        continue;
                    }
                },
                None => tasks.join_next_with_id().await,
            };
            let Some(joined) = joined else { break };
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
                    let id = frontier.known[index].id;
                    writer.send(Write::Response { id, http, internal, depth: None }).await?;
                    continue;
                }
            };
            if outcome.item.internal {
                internal_in_flight -= 1;
            } else {
                external_in_flight -= 1;
            }
            if outcome.unsent {
                continue;
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
            record(&writer, outcome, config.store.links, &mut frontier, &mut summary).await?;
            storing += stored.elapsed();
            if let Some(progress) = progress.as_ref().filter(|_| last_report.elapsed() >= REPORT_EVERY) {
                last_report = Instant::now();
                let paused = pause.load(Ordering::Relaxed);
                progress.send_replace(progress_of(&summary, frontier.queued(), tasks.len(), started, paused));
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
        progress.send_replace(progress_of(&summary, 0, 0, started, false));
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
        .filter(|(known, _)| known.saved)
        .map(|(known, depth)| (known.id, depth, in_sitemap.contains(&known.url), known.blocked))
        .collect();
    let states_done = Instant::now();
    store.set_crawl_states(&states)?;
    let saved = Instant::now();
    store.finish_crawl(crawl_id, summary.stopped)?;
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
    /// Set once the crawl is to stop: requests still waiting their turn
    /// are not sent.
    stop: Arc<AtomicBool>,
    /// Find each page's issues as it is crawled.
    live_issues: bool,
    /// The sitemap URLs, for the live checks; set once the sitemaps are read.
    in_sitemap: std::sync::OnceLock<HashSet<String>>,
    /// Extra attempts for a request that failed in a way worth retrying.
    retries: u32,
    /// What is read from the raw document: everything asked for when it is
    /// fetched whole, the `<head>` otherwise.
    raw_extract: ExtractOptions,
    /// What is read from the rendered DOM.
    extract: ExtractOptions,
    /// Custom extraction and search, when any is set.
    custom: Option<seo_page::CustomRules>,
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

/// Spaces requests to the site an interval apart: max_rps or robots.txt
/// Crawl-delay at least, and further apart while the site answers 429 or
/// 503: each such answer doubles the interval (from 100 ms, up to 10 s),
/// each response that goes through takes a 32nd off it.
struct Pacer {
    floor: Duration,
    adaptive: bool,
    state: std::sync::Mutex<PacerState>,
}

struct PacerState {
    next: tokio::time::Instant,
    interval: Duration,
    /// Answers to requests sent before the last slow-down say nothing new.
    slowed_at: Option<tokio::time::Instant>,
}

impl Pacer {
    fn new(floor: Duration, adaptive: bool) -> Self {
        let state = PacerState { next: tokio::time::Instant::now(), interval: floor, slowed_at: None };
        Pacer { floor, adaptive, state: std::sync::Mutex::new(state) }
    }

    /// Waits for this request's turn; false if the crawl stops first, which
    /// behind a Crawl-delay of 10 s can be minutes away.
    async fn pace(&self, stop: &AtomicBool) -> bool {
        let at = {
            let mut state = self.state.lock().expect("pacer lock");
            let at = state.next.max(tokio::time::Instant::now());
            state.next = at + state.interval;
            at
        };
        sleep_unless_stopped(stop, at).await
    }

    /// The site answered 429 or 503 to a request sent at `sent`.
    fn slow_down(&self, sent: tokio::time::Instant) {
        if !self.adaptive {
            return;
        }
        let mut state = self.state.lock().expect("pacer lock");
        if state.slowed_at.is_some_and(|at| sent <= at) {
            return;
        }
        state.slowed_at = Some(tokio::time::Instant::now());
        state.interval = (state.interval * 2).clamp(Duration::from_millis(100), Duration::from_secs(10)).max(self.floor);
        tracing::info!(interval_ms = state.interval.as_millis() as u64, "the site asks to slow down");
    }

    /// A response that went through eases the interval back to the floor.
    fn speed_up(&self) {
        if !self.adaptive {
            return;
        }
        let mut state = self.state.lock().expect("pacer lock");
        if state.interval > self.floor {
            let eased = state.interval - state.interval / 32;
            state.interval = if eased < Duration::from_millis(1) { self.floor } else { eased.max(self.floor) };
        }
    }

    #[cfg(test)]
    fn interval(&self) -> Duration {
        self.state.lock().unwrap().interval
    }
}

/// Sleeps until `until`, looking at `stop` every 100 ms; false if it was set.
async fn sleep_unless_stopped(stop: &AtomicBool, until: tokio::time::Instant) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= until {
            return true;
        }
        tokio::time::sleep_until(until.min(now + Duration::from_millis(100))).await;
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
    /// The crawl stopped before the request was sent: the URL stays
    /// unrequested, as the queued ones do.
    unsent: bool,
    /// The page's issues, when the crawl finds them as it goes.
    issues: Vec<(&'static str, Option<String>)>,
}

impl Outcome {
    fn unsent(item: Item) -> Self {
        Outcome {
            http: HttpInfo { url: item.url.to_string(), ..Default::default() },
            item,
            bytes_read: 0,
            raw: None,
            raw_body: None,
            rendered: None,
            candidates: Vec::new(),
            unsent: true,
            issues: Vec::new(),
        }
    }
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
        let Some(fetched) = fetch_retrying(&ctx, &item, ctx.body, None).await else {
            return Outcome::unsent(item);
        };
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
                        let Some(fetched) =
                            fetch_retrying(&ctx, &item, fetch::BodyMode::None, Some(ctx.external_timeout)).await
                        else {
                            return Outcome::unsent(item);
                        };
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
    let bytes_read = body.as_ref().map_or(0, |body| body.read);
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
        let mut facts = seo_page::extract_with(&body.html, &item.url, Source::Raw, &ctx.raw_extract);
        if let Some(rules) = &ctx.custom {
            facts.custom = rules.run(&body.html);
        }
        if ctx.store.raw_html {
            raw_body = Some(pack(body.bytes, ctx.store.compression_level));
        }
        document(facts, ctx.store.compression_level)
    });
    let rendered = match &ctx.render {
        Some(pool) if renderable => Some(render_page(pool, &item.url, downloaded, &ctx).await),
        _ => None,
    };
    let mut outcome =
        Outcome { item, http, bytes_read, raw, raw_body, rendered, candidates: Vec::new(), unsent: false, issues: Vec::new() };
    outcome.candidates = discover(&outcome, &ctx.rules);
    if ctx.live_issues && outcome.item.internal {
        outcome.issues = live_issues(&outcome, &ctx);
    }
    outcome
}

/// A page's issues as `check_page` finds them from this page alone, for a
/// live view; the checks after the crawl, which see every page, replace them.
fn live_issues(outcome: &Outcome, ctx: &Ctx) -> Vec<(&'static str, Option<String>)> {
    let raw = outcome.raw.as_ref().map(|document| &document.facts);
    let rendered = outcome.rendered.as_ref().and_then(|page| page.document.as_ref()).map(|document| &document.facts);
    let render = outcome.rendered.as_ref().map(|page| {
        let record = &page.record;
        seo_store::render_summary(record.error.clone(), record.settled, &record.console_errors_json, &record.resources_json)
    });
    let facts = rendered.or(raw);
    let input = seo_checks::PageInput {
        url: &outcome.item.url,
        http: &outcome.http,
        facts,
        raw,
        rendered,
        render: render.as_ref(),
        depth: outcome.item.depth,
        in_sitemap: ctx.in_sitemap.get().is_some_and(|listed| listed.contains(outcome.item.url.as_str())),
        blocked_by_robots: false,
    };
    let extracted = facts.map_or_else(ExtractOptions::default, |facts| facts.extracted);
    seo_checks::check_page(&input)
        .into_iter()
        .filter(|issue| audit::meaningful(issue.code, &extracted))
        .map(|issue| (issue.code, issue.detail))
        .collect()
}

type Pair = (String, String);

/// The crawl's extra headers as the renderer takes them: the site's cookies
/// go into its jar for the start hosts (gazeta.ru and lenta.ru pass their
/// Sber ID redirect on one); other headers go with every request, so
/// credentials stay with the crawler: they would reach third-party hosts.
fn render_headers(headers: &BTreeMap<String, String>, start_urls: &[Url]) -> (Vec<Pair>, Vec<Pair>) {
    let (mut cookies, mut sent) = (Vec::new(), Vec::new());
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("cookie") {
            for start in start_urls {
                for pair in value.split(';').map(str::trim).filter(|pair| !pair.is_empty()) {
                    cookies.push((start.to_string(), pair.to_string()));
                }
            }
        } else if !name.eq_ignore_ascii_case("authorization") && !name.eq_ignore_ascii_case("proxy-authorization") {
            sent.push((name.clone(), value.clone()));
        }
    }
    (cookies, sent)
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

/// Requests the item, again while the failure is one worth retrying; `None`
/// when the crawl stopped before the request was sent.
async fn fetch_retrying(
    ctx: &Ctx,
    item: &Item,
    mode: fetch::BodyMode,
    timeout: Option<Duration>,
) -> Option<fetch::Fetched> {
    // External URLs are only retried after a network failure: a third
    // party answering 5xx is the answer worth reporting.
    let retries = if item.internal { ctx.retries } else { ctx.retries.min(1) };
    let mut attempt = 0;
    loop {
        if ctx.stop.load(Ordering::Relaxed) {
            return None;
        }
        if item.internal {
            if let Some(pacer) = &ctx.pacer {
                if !pacer.pace(&ctx.stop).await {
                    return None;
                }
            }
        }
        let client = if item.internal { &ctx.client } else { &ctx.external_client };
        let sent = tokio::time::Instant::now();
        let fetched = fetch::fetch(client, &item.url, mode, timeout, ctx.head_for_status).await;
        // Connect timeouts are no signal: mos.ru, refusing connections after
        // a burst, went on refusing them at a 10 s interval.
        if let (true, Some(pacer)) = (item.internal, &ctx.pacer) {
            match fetched.http.status {
                429 | 503 => pacer.slow_down(sent),
                200..=399 => pacer.speed_up(),
                _ => {}
            }
        }
        match retry_delay(&fetched.http, item.internal, attempt) {
            Some(delay) if attempt < retries => {
                tracing::debug!(url = %item.url, attempt, status = fetched.http.status, "retrying");
                attempt += 1;
                if !sleep_unless_stopped(&ctx.stop, tokio::time::Instant::now() + delay).await {
                    return Some(fetched);
                }
            }
            _ => return Some(fetched),
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
        let backoff = tokio::time::Instant::now() + Duration::from_millis(500 << attempt.min(5));
        if !sleep_unless_stopped(&ctx.stop, backoff).await {
            break rendered;
        }
        attempt += 1;
        if let Some(pacer) = &ctx.pacer {
            if !pacer.pace(&ctx.stop).await {
                break rendered;
            }
        }
    };
    let Rendered { final_url, error, html, dcl_ms, total_ms, settled, load_fired, resources, console_errors, .. } =
        rendered;
    let (document, body) = if error.is_none() {
        let base = Url::parse(&final_url).unwrap_or_else(|_| url.clone());
        let mut facts = seo_page::extract_with(&html, &base, Source::Rendered, &ctx.extract);
        if let Some(rules) = &ctx.custom {
            facts.custom = rules.run(&html);
        }
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
    outcome: Outcome,
    links: bool,
    frontier: &mut Frontier,
    summary: &mut CrawlSummary,
) -> Result<()> {
    let Outcome { item, http, bytes_read, raw, raw_body, rendered, issues, .. } = outcome;
    let url_id = frontier.known[item.index].id;
    frontier.known[item.index].saved = true;
    summary.urls += 1;
    if http.status == 0 || http.status >= 400 {
        summary.errors += 1;
    }
    summary.bytes += bytes_read;
    writer.send(Write::Response { id: url_id, http, internal: item.internal, depth: Some(item.depth) }).await?;
    if !issues.is_empty() {
        writer.send(Write::Issues { url_id, issues }).await?;
    }
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
    let documents = [
        outcome.raw.as_ref().map(|document| &document.facts),
        outcome.rendered.as_ref().and_then(|page| page.document.as_ref()).map(|document| &document.facts),
    ];
    discover_from(outcome.item.depth, outcome.item.resource, &outcome.http, documents, rules)
}

/// `discover` from the parts of an outcome, as a continued crawl has them
/// from the store.
fn discover_from(
    depth: u32,
    from_resource: bool,
    http: &HttpInfo,
    documents: [Option<&PageFacts>; 2],
    rules: &Rules,
) -> Vec<Candidate> {
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
    if http.is_redirect() {
        if let Some(location) = &http.location {
            queue(location, depth, Edge::Redirect);
        }
    }
    // Only internal documents are read, so their links are followed.
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
    /// Its row's id in the store.
    id: i64,
    /// The crawl wrote a row for it.
    saved: bool,
    blocked: bool,
    /// (target, edge) for every click, redirect and resource from this URL.
    edges: Vec<(u32, Edge)>,
}

/// URLs known to the crawl in discovery order and the ones still to fetch.
/// A new URL's id in the store is the next one free, so a fresh crawl
/// numbers its URLs in the order it found them.
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
    next_id: i64,
}

impl Frontier {
    fn new(max_urls: usize, max_depth: Option<u32>, first_id: i64) -> Self {
        Frontier {
            pages: VecDeque::new(),
            resources: VecDeque::new(),
            external: VecDeque::new(),
            index: FxHashMap::default(),
            known: Vec::new(),
            counts: [0; 3],
            max_urls,
            max_depth,
            next_id: first_id,
        }
    }

    fn budget(internal: bool, resource: bool) -> usize {
        match (internal, resource) {
            (true, false) => 0,
            (true, true) => 1,
            (false, _) => 2,
        }
    }

    /// A URL a continued crawl wrote before: known, with its id, counted in
    /// its budget whatever the limit, and not queued.
    fn register(&mut self, url: String, id: i64, internal: bool, resource: bool, blocked: bool) -> usize {
        if let Some(&index) = self.index.get(&url) {
            return index;
        }
        self.counts[Self::budget(internal, resource)] += 1;
        let index = self.known.len();
        self.index.insert(url.clone(), index);
        self.known.push(Known { url, id, saved: true, blocked, edges: Vec::new() });
        index
    }

    /// Queues again a page `register` knew, to be requested under its id.
    fn requeue(&mut self, index: usize, depth: u32) {
        let Ok(url) = Url::parse(&self.known[index].url) else { return };
        self.known[index].saved = false;
        self.pages.push_back(Item { url, index, depth, internal: true, resource: false });
    }

    fn push(&mut self, url: Url, depth: u32, internal: bool, resource: bool, from: Option<usize>, edge: Edge) {
        let index = match self.index.get(url.as_str()) {
            Some(&index) => index,
            None => {
                let budget = Self::budget(internal, resource);
                if self.counts[budget] >= self.max_urls || self.max_depth.is_some_and(|max| depth > max) {
                    return;
                }
                self.counts[budget] += 1;
                let index = self.known.len();
                self.index.insert(url.to_string(), index);
                let id = self.next_id;
                self.next_id += 1;
                self.known.push(Known { url: url.to_string(), id, saved: false, blocked: false, edges: Vec::new() });
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

    /// Serves `pages` by path over HTTP/1.1 (404 for anything else) and
    /// counts the page requests (robots.txt aside).
    async fn site(pages: Vec<(&'static str, String)>) -> (Url, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = requests.clone();
        let pages = Arc::new(pages);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (pages, counted) = (pages.clone(), counted.clone());
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 4096];
                    loop {
                        let Ok(read) = socket.read(&mut buffer).await else { return };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") else { continue };
                        let head = String::from_utf8_lossy(&request[..end]).to_string();
                        request.drain(..end + 4);
                        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                        if path != "/robots.txt" {
                            counted.fetch_add(1, Ordering::SeqCst);
                        }
                        let (status, body) = match pages.iter().find(|(have, _)| *have == path) {
                            Some((_, body)) => ("200 OK", body.clone()),
                            None => ("404 Not Found", "<html><body>404</body></html>".to_string()),
                        };
                        let reply = format!(
                            "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        if socket.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (url, requests)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_paused_crawl_waits_and_live_issues_are_stored() {
        let page = |title: &str, links: &str| {
            format!("<html lang=en><head><title>{title}</title></head><body><h1>{title}</h1>{links}</body></html>")
        };
        let (start, requests) = site(vec![
            ("/", page("Home page of the test", "<a href=/a>a</a><a href=/b>b</a>")),
            ("/a", page("Page a of the test site", "<a href=/>home</a>")),
            ("/b", "<html><body><a href=/>home</a></body></html>".to_string()),
        ])
        .await;
        let path = std::env::temp_dir().join(format!("seo-crawl-pause-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let mut config = CrawlConfig::new(start);
        config.check_external = false;
        config.check_resources = false;
        let pause = Arc::new(AtomicBool::new(true));
        let (progress, mut seen) = tokio::sync::watch::channel(CrawlProgress::default());
        let control =
            CrawlControl { stop: Arc::default(), pause: pause.clone(), progress: Some(progress), live_issues: true };
        let crawl = tokio::spawn(crawl_with(config, store, control));
        while !seen.borrow_and_update().paused {
            seen.changed().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(requests.load(Ordering::SeqCst), 0, "nothing is requested while paused");
        assert_eq!(seen.borrow().urls, 0);
        pause.store(false, Ordering::SeqCst);
        let (store, summary) = crawl.await.unwrap().unwrap();
        assert_eq!(summary.urls, 3);
        let missing: Vec<String> = store
            .query_rows("SELECT u.url FROM issues i JOIN urls u ON u.id = i.url_id WHERE i.code = 'title_missing'", &[])
            .unwrap()
            .iter()
            .map(|row| row.get(0).unwrap())
            .collect();
        assert_eq!(missing, [format!("{}b", summary_start(&store))]);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    /// A small site: 8 pages and a 404, three clicks deep.
    fn tree() -> Vec<(&'static str, String)> {
        let page = |links: &[&str]| {
            let links: String = links.iter().map(|href| format!("<a href={href}>{href}</a>")).collect();
            format!("<html><head><title>Page</title></head><body>{links}</body></html>")
        };
        vec![
            ("/", page(&["/a", "/b", "/c"])),
            ("/a", page(&["/a1", "/a2", "/"])),
            ("/b", page(&["/b1", "/missing"])),
            ("/c", page(&["/"])),
            ("/a1", page(&["/deep"])),
            ("/a2", page(&[])),
            ("/b1", page(&["/a1"])),
            ("/deep", page(&[])),
        ]
    }

    fn slow_config(start: Url) -> CrawlConfig {
        let mut config = CrawlConfig::new(start);
        config.check_external = false;
        config.check_resources = false;
        // A request every 60 ms: the crawl is still going when it is cut.
        config.http.max_rps = Some(16.0);
        config
    }

    /// (path, depth, has facts) of every URL of the crawl, by path.
    fn crawled(store: &Store, crawl_id: i64) -> Vec<(String, Option<u32>, bool)> {
        let mut rows: Vec<(String, Option<u32>, bool)> = store
            .query_rows(
                "SELECT u.url, u.depth, (SELECT count(*) FROM facts f WHERE f.url_id = u.id)
                 FROM urls u WHERE u.crawl_id = ?1",
                &seo_store::params![crawl_id],
            )
            .unwrap()
            .iter()
            .map(|row| (row.get::<String>(0).unwrap(), row.get(1).unwrap(), row.get(2).unwrap()))
            .collect();
        for row in &mut rows {
            row.0 = Url::parse(&row.0).unwrap().path().to_string();
        }
        rows.sort();
        rows
    }

    /// A crawl cut short, stopped or killed, and then continued ends as the
    /// same crawl run at one go: the same URLs at the same depths, none of
    /// them requested twice (but a page whose facts were lost).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_continued_crawl_ends_as_one_run_would() {
        let temp = |name: &str| {
            let path = std::env::temp_dir().join(format!("seo-crawl-{name}-{}.sqlite", std::process::id()));
            let _ = std::fs::remove_file(&path);
            path.to_string_lossy().into_owned()
        };
        let files = [temp("whole"), temp("stopped"), temp("killed")];
        continued_crawl(&files).await;
        for file in files {
            let _ = std::fs::remove_file(file);
        }
    }

    /// The same on the database servers of `SEO_SPIDER_TEST_POSTGRES` and
    /// `SEO_SPIDER_TEST_MYSQL`, when given.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_continued_crawl_on_servers() {
        for name in ["SEO_SPIDER_TEST_POSTGRES", "SEO_SPIDER_TEST_MYSQL"] {
            if let Ok(target) = std::env::var(name) {
                continued_crawl(&[target.clone(), target.clone(), target]).await;
            }
        }
    }

    /// Crawls the tree whole into `targets[0]`, then stopped and continued
    /// into `targets[1]`, then killed and continued into `targets[2]`.
    async fn continued_crawl(targets: &[String; 3]) {
        let one = |store: &Store, sql: &str, crawl_id: i64| -> seo_store::Row {
            store.query_row(sql, &seo_store::params![crawl_id]).unwrap().unwrap()
        };
        let (start, _) = site(tree()).await;
        let (whole, summary) = crawl(slow_config(start), Store::open(&targets[0]).unwrap()).await.unwrap();
        let expected = crawled(&whole, summary.crawl_id);
        assert_eq!(expected.len(), 9);
        drop(whole);

        // Stopped once three URLs are in, then continued.
        let (start, requests) = site(tree()).await;
        let stop = Arc::new(AtomicBool::new(false));
        let (progress, mut seen) = tokio::sync::watch::channel(CrawlProgress::default());
        let control = CrawlControl { stop: stop.clone(), progress: Some(progress), ..Default::default() };
        let first = tokio::spawn(crawl_with(slow_config(start.clone()), Store::open(&targets[1]).unwrap(), control));
        while seen.borrow_and_update().urls < 3 {
            seen.changed().await.unwrap();
        }
        stop.store(true, Ordering::SeqCst);
        let (store, cut) = first.await.unwrap().unwrap();
        assert!(cut.stopped && cut.urls < 9, "the crawl was cut: {} URLs", cut.urls);
        let stopped: bool = one(&store, "SELECT stopped FROM crawls WHERE id = ?1", cut.crawl_id).get(0).unwrap();
        assert!(stopped);
        let config: CrawlConfig = serde_json::from_str(&store.crawl_config(cut.crawl_id).unwrap()).unwrap();
        let (store, done) = continue_with(config, store, cut.crawl_id, CrawlControl::default()).await.unwrap();
        assert_eq!((done.crawl_id, done.urls, done.stopped), (cut.crawl_id, 9, false));
        assert_eq!(crawled(&store, done.crawl_id), expected);
        assert_eq!(requests.load(Ordering::SeqCst), 9, "no URL is requested twice");
        let row = one(&store, "SELECT finished_at, stopped FROM crawls WHERE id = ?1", done.crawl_id);
        assert!(row.get::<Option<String>>(0).unwrap().is_some() && !row.get::<bool>(1).unwrap());
        drop(store);

        // Killed mid-crawl: nothing closes it, and the start page's facts
        // are lost as if the run ended between its two writes.
        let (start, requests) = site(tree()).await;
        let (progress, mut seen) = tokio::sync::watch::channel(CrawlProgress::default());
        let control = CrawlControl { progress: Some(progress), ..Default::default() };
        let killed = tokio::spawn(crawl_with(slow_config(start.clone()), Store::open(&targets[2]).unwrap(), control));
        while seen.borrow_and_update().urls < 3 {
            seen.changed().await.unwrap();
        }
        let crawl_id = seen.borrow().crawl_id;
        killed.abort();
        let _ = killed.await;
        // The writer thread applies what it was handed and lets go.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let store = Store::open(&targets[2]).unwrap();
        let row = one(&store, "SELECT (SELECT count(*) FROM urls WHERE crawl_id = ?1), finished_at FROM crawls WHERE id = ?1", crawl_id);
        let (written, finished): (usize, Option<String>) = (row.get(0).unwrap(), row.get(1).unwrap());
        assert!(written < 9 && finished.is_none(), "{written} URLs written, finished {finished:?}");
        store
            .execute(
                "DELETE FROM facts WHERE url_id = (SELECT id FROM urls WHERE crawl_id = ?1 AND url = ?2)",
                &seo_store::params![crawl_id, start.as_str()],
            )
            .unwrap();
        let config: CrawlConfig = serde_json::from_str(&store.crawl_config(crawl_id).unwrap()).unwrap();
        let (store, done) = continue_with(config, store, crawl_id, CrawlControl::default()).await.unwrap();
        assert_eq!(done.urls, 9);
        assert_eq!(crawled(&store, crawl_id), expected);
        assert_eq!(requests.load(Ordering::SeqCst), 10, "only the page without facts is requested again");
    }

    fn summary_start(store: &Store) -> String {
        store.query_row("SELECT start_url FROM crawls", &[]).unwrap().unwrap().get(0).unwrap()
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn render_gets_site_cookies_but_no_credentials() {
        let headers: BTreeMap<String, String> = [
            ("Cookie", "unity_pause_sso=1; theme=dark"),
            ("Authorization", "Basic c2VjcmV0"),
            ("X-Test", "yes"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
        let start = [url("https://www.gazeta.ru/")];
        let (cookies, sent) = render_headers(&headers, &start);
        assert_eq!(
            cookies,
            [
                ("https://www.gazeta.ru/".to_string(), "unity_pause_sso=1".to_string()),
                ("https://www.gazeta.ru/".to_string(), "theme=dark".to_string()),
            ]
        );
        assert_eq!(sent, [("X-Test".to_string(), "yes".to_string())]);
    }

    #[tokio::test]
    async fn pacer_backs_off_on_overload_and_eases_back() {
        let pacer = Pacer::new(Duration::ZERO, true);
        let sent = tokio::time::Instant::now();
        pacer.slow_down(sent);
        assert_eq!(pacer.interval(), Duration::from_millis(100));
        // Other answers to requests sent before it do not compound it; an
        // answer to one sent after it does.
        pacer.slow_down(sent);
        assert_eq!(pacer.interval(), Duration::from_millis(100));
        tokio::time::sleep(Duration::from_millis(2)).await;
        pacer.slow_down(tokio::time::Instant::now());
        assert_eq!(pacer.interval(), Duration::from_millis(200));
        for _ in 0..300 {
            pacer.speed_up();
        }
        assert_eq!(pacer.interval(), Duration::ZERO);
        let floored = Pacer::new(Duration::from_millis(500), true);
        floored.slow_down(tokio::time::Instant::now());
        assert_eq!(floored.interval(), Duration::from_secs(1));
        for _ in 0..300 {
            floored.speed_up();
        }
        assert_eq!(floored.interval(), Duration::from_millis(500));
        let fixed = Pacer::new(Duration::from_millis(500), false);
        fixed.slow_down(tokio::time::Instant::now());
        assert_eq!(fixed.interval(), Duration::from_millis(500));
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
        let mut frontier = Frontier::new(100, None, 1);
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
        let mut frontier = Frontier::new(2, Some(1), 1);
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
