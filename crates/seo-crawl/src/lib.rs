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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use regex::RegexSet;
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
    /// Bytes of response bodies read.
    pub bytes: u64,
    pub elapsed: Duration,
}

/// Crawls into `store` as `config` says, then hands the store back.
pub async fn crawl(config: CrawlConfig, store: Store) -> Result<(Store, CrawlSummary)> {
    let started = Instant::now();
    if config.start_urls.is_empty() {
        bail!("no start URL");
    }
    let client = fetch::client(&fetch::ClientOptions {
        user_agent: &config.http.user_agent,
        timeout: config.http.timeout(),
        concurrency: config.http.concurrency,
        headers: &config.http.headers,
    })?;
    let crawl_id = store.start_crawl(config.start_urls[0].as_str(), &serde_json::to_string(&config)?)?;
    let base_id = store.next_url_id()?;
    let writer = writer::Writer::spawn(store, crawl_id)?;

    let mut robots = robots::Robots::new(&config.http.robots_agent);
    for start in &config.start_urls {
        robots.load(&client, start).await;
    }

    let render = config.render.as_ref().map(|settings| RenderPool::new(settings.to_render_config()));
    let render_threads = config.render.as_ref().map_or(0, |settings| settings.threads);
    let concurrency = config.http.concurrency.max(1);
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        render,
        http_slots: Semaphore::new(concurrency),
        external_slots: Semaphore::new(concurrency),
        external_timeout: config.http.timeout().min(config.http.external_timeout()),
        pacer: config.http.max_rps.filter(|rps| *rps > 0.0).map(Pacer::new),
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
    // Pages waiting for a render thread keep their HTTP slot free, so allow
    // a few of them per thread on top of the requests.
    let max_in_flight = concurrency * 2 + render_threads * 2;

    let mut frontier = Frontier::new(config.max_urls, config.max_depth);
    for start in &config.start_urls {
        if let Some(url) = rules.filters.normalize(start.clone()) {
            let internal = rules.scope.internal(&url);
            frontier.push(url, 0, internal, None, Edge::Other);
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
                frontier.push(url, 1, true, None, Edge::Other);
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
    };
    let mut tasks = JoinSet::new();
    let mut last_progress = Instant::now();
    // Where the dispatcher's time goes: it is the one task every outcome
    // passes through.
    let (mut waiting, mut storing, mut discovering) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let result: Result<()> = async {
        loop {
            while tasks.len() < max_in_flight {
                let Some(item) = frontier.next() else { break };
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
                tasks.spawn(process(ctx.clone(), item));
            }
            let waited = Instant::now();
            let Some(joined) = tasks.join_next().await else { break };
            waiting += waited.elapsed();
            let mut outcome = match joined {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::error!(%error, "crawl task failed");
                    continue;
                }
            };
            let discovered = Instant::now();
            for candidate in std::mem::take(&mut outcome.candidates) {
                frontier.push(candidate.url, candidate.depth, candidate.internal, Some(outcome.item.index), candidate.edge);
            }
            discovering += discovered.elapsed();
            let stored = Instant::now();
            record(&writer, base_id, outcome, config.store.links, &mut frontier, &mut summary).await?;
            storing += stored.elapsed();
            if last_progress.elapsed() >= Duration::from_secs(2) {
                last_progress = Instant::now();
                tracing::info!(
                    done = summary.urls,
                    pages = summary.pages,
                    queued = frontier.queue.len(),
                    in_flight = tasks.len(),
                    "crawling"
                );
            }
        }
        Ok(())
    }
    .await;
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
    store.set_crawl_states(&states)?;
    store.finish_crawl(crawl_id)?;
    summary.elapsed = started.elapsed();
    Ok((store, summary))
}

struct Ctx {
    client: reqwest::Client,
    render: Option<RenderPool>,
    http_slots: Semaphore,
    external_slots: Semaphore,
    external_timeout: Duration,
    pacer: Option<Pacer>,
    body: fetch::BodyMode,
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
    edge: Edge,
}

/// Spaces requests to the site `1 / max_rps` apart.
struct Pacer {
    interval: Duration,
    next: std::sync::Mutex<tokio::time::Instant>,
}

impl Pacer {
    fn new(max_rps: f64) -> Self {
        Pacer {
            interval: Duration::from_secs_f64(1.0 / max_rps),
            next: std::sync::Mutex::new(tokio::time::Instant::now()),
        }
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
}

/// A document read, with what the store needs prepared.
struct Document {
    facts: PageFacts,
    data: Vec<u8>,
}

struct Outcome {
    item: Item,
    http: HttpInfo,
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
    let (http, body) = {
        let (slots, timeout) = if item.internal {
            (&ctx.http_slots, None)
        } else {
            (&ctx.external_slots, Some(ctx.external_timeout))
        };
        let _slot = slots.acquire().await.expect("HTTP slots are never closed");
        if item.internal {
            if let Some(pacer) = &ctx.pacer {
                pacer.pace().await;
            }
        }
        let mode = if item.internal { ctx.body } else { fetch::BodyMode::None };
        fetch::fetch(&ctx.client, &item.url, mode, timeout).await
    };
    // Everything below is CPU work, done here on a runtime worker so it runs
    // in parallel across pages instead of on the dispatcher.
    let mut raw_body = None;
    let raw = body.map(|body| {
        let facts = seo_page::extract_with(&body.html, &item.url, Source::Raw, &ctx.raw_extract);
        if ctx.store.raw_html {
            raw_body = Some(pack(body.bytes, ctx.store.compression_level));
        }
        document(facts, ctx.store.compression_level)
    });
    let renderable = item.internal && http.is_html() && (200..300).contains(&http.status);
    let rendered = match &ctx.render {
        Some(pool) if renderable => Some(render_page(pool, &item.url, &ctx).await),
        _ => None,
    };
    let mut outcome = Outcome { item, http, raw, raw_body, rendered, candidates: Vec::new() };
    outcome.candidates = discover(&outcome, &ctx.rules);
    outcome
}

async fn render_page(pool: &RenderPool, url: &Url, ctx: &Ctx) -> RenderedPage {
    let Rendered { final_url, error, html, dcl_ms, total_ms, settled, load_fired, resources, console_errors, .. } =
        pool.render(url.to_string()).await;
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
    let Outcome { item, http, raw, raw_body, rendered, .. } = outcome;
    let url_id = base_id + item.index as i64;
    frontier.known[item.index].saved = true;
    summary.urls += 1;
    if http.status == 0 || http.status >= 400 {
        summary.errors += 1;
    }
    summary.bytes += http.size;
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
    let mut seen = HashSet::new();
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
        candidates.push(Candidate { url, depth, internal, edge });
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
    queue: VecDeque<Item>,
    index: HashMap<String, usize>,
    known: Vec<Known>,
    internal: usize,
    external: usize,
    max_urls: usize,
    max_depth: Option<u32>,
}

impl Frontier {
    fn new(max_urls: usize, max_depth: Option<u32>) -> Self {
        Frontier {
            queue: VecDeque::new(),
            index: HashMap::new(),
            known: Vec::new(),
            internal: 0,
            external: 0,
            max_urls,
            max_depth,
        }
    }

    fn push(&mut self, url: Url, depth: u32, internal: bool, from: Option<usize>, edge: Edge) {
        let index = match self.index.get(url.as_str()) {
            Some(&index) => index,
            None => {
                let count = if internal { &mut self.internal } else { &mut self.external };
                if *count >= self.max_urls || self.max_depth.is_some_and(|max| depth > max) {
                    return;
                }
                *count += 1;
                let index = self.known.len();
                self.index.insert(url.to_string(), index);
                self.known.push(Known { url: url.to_string(), saved: false, blocked: false, edges: Vec::new() });
                self.queue.push_back(Item { url, index, depth, internal });
                index
            }
        };
        if let Some(from) = from.filter(|&from| from != index && edge != Edge::Other) {
            self.known[from].edges.push((index as u32, edge));
        }
    }

    fn next(&mut self) -> Option<Item> {
        self.queue.pop_front()
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
        frontier.push(url("https://e.com/"), 0, true, None, Edge::Other);
        frontier.push(url("https://e.com/a"), 1, true, Some(0), Edge::Click);
        frontier.push(url("https://e.com/b"), 2, true, Some(1), Edge::Click);
        frontier.push(url("https://e.com/b"), 1, true, Some(0), Edge::Click);
        frontier.push(url("https://e.com/canonical"), 1, true, Some(0), Edge::Other);
        frontier.push(url("https://e.com/orphan"), 1, true, None, Edge::Other);
        frontier.push(url("https://e.com/moved"), 2, true, Some(1), Edge::Redirect);
        frontier.push(url("https://e.com/img.png"), 2, true, Some(1), Edge::Resource);
        assert_eq!(frontier.known.len(), 7);
        assert_eq!(
            frontier.depths(&[0]),
            vec![Some(0), Some(1), Some(1), None, None, Some(1), Some(2)]
        );
    }

    #[test]
    fn limits_stop_queueing() {
        let mut frontier = Frontier::new(2, Some(1));
        frontier.push(url("https://e.com/"), 0, true, None, Edge::Other);
        frontier.push(url("https://e.com/deep"), 2, true, Some(0), Edge::Click);
        frontier.push(url("https://e.com/a"), 1, true, Some(0), Edge::Click);
        frontier.push(url("https://e.com/b"), 1, true, Some(0), Edge::Click);
        frontier.push(url("https://other.com/"), 1, false, Some(0), Edge::Click);
        assert_eq!(frontier.known.len(), 3);
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
    }
}
