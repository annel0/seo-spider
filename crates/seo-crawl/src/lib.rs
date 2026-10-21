//! Crawl orchestration. One dispatcher owns the frontier and the store; each
//! URL becomes a task that fetches it without following redirects (bounded by
//! the HTTP slots), renders HTML pages on the obscura pool and extracts facts
//! from both documents. The dispatcher stores each outcome and queues the
//! URLs it found, so redirect targets and links that only exist after
//! JavaScript ran are crawled like any other.

mod audit;
mod fetch;
mod robots;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use seo_page::{HttpInfo, PageFacts, Source};
use seo_render::{RenderPool, Rendered};
use seo_store::{RenderRow, Store};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;

pub use audit::run_checks;
pub use seo_render::{RenderConfig, SettlePolicy};

#[derive(Debug, Clone)]
pub struct CrawlConfig {
    pub start_url: Url,
    /// Stop queueing new internal URLs once this many are known; external
    /// URLs have a budget of the same size of their own.
    pub max_urls: usize,
    /// URLs more than this many steps from the start are not queued.
    pub max_depth: Option<u32>,
    /// HTTP requests in flight to the site. External checks get as many
    /// slots of their own (spread over many hosts) and a shorter timeout,
    /// so unreachable third parties do not hold the crawl.
    pub concurrency: usize,
    /// Render HTML pages with JavaScript; `None` reads the raw HTML only.
    pub render: Option<RenderConfig>,
    pub user_agent: String,
    /// Product token robots.txt groups are matched against.
    pub robots_agent: String,
    /// Skip URLs robots.txt disallows (they are still reported).
    pub respect_robots: bool,
    pub follow_nofollow: bool,
    /// Subdomains of the start host count as internal.
    pub crawl_subdomains: bool,
    /// Request external links once to report their status.
    pub check_external: bool,
    /// Request images, scripts, stylesheets and media to report their status.
    pub check_resources: bool,
    /// Queue the URLs of the sitemaps listed in robots.txt (or /sitemap.xml).
    pub use_sitemaps: bool,
    pub timeout: Duration,
    /// HTML bodies are cut at this size.
    pub max_body: usize,
}

impl CrawlConfig {
    pub fn new(start_url: Url) -> Self {
        CrawlConfig {
            start_url,
            max_urls: 10_000,
            max_depth: None,
            concurrency: 16,
            render: None,
            user_agent: concat!("Mozilla/5.0 (compatible; seo-spider/", env!("CARGO_PKG_VERSION"), ")").into(),
            robots_agent: "seo-spider".into(),
            respect_robots: true,
            follow_nofollow: false,
            crawl_subdomains: false,
            check_external: true,
            check_resources: true,
            use_sitemaps: false,
            timeout: Duration::from_secs(20),
            max_body: 10 << 20,
        }
    }

    fn to_json(&self) -> String {
        serde_json::json!({
            "start_url": self.start_url.as_str(),
            "max_urls": self.max_urls,
            "max_depth": self.max_depth,
            "concurrency": self.concurrency,
            "render_threads": self.render.as_ref().map(|render| render.threads),
            "user_agent": self.user_agent,
            "robots_agent": self.robots_agent,
            "respect_robots": self.respect_robots,
            "follow_nofollow": self.follow_nofollow,
            "crawl_subdomains": self.crawl_subdomains,
            "check_external": self.check_external,
            "check_resources": self.check_resources,
            "use_sitemaps": self.use_sitemaps,
            "timeout_ms": self.timeout.as_millis() as u64,
            "max_body": self.max_body,
        })
        .to_string()
    }
}

#[derive(Debug, Clone)]
pub struct CrawlSummary {
    pub crawl_id: i64,
    /// Every URL the crawl recorded, redirects and resources included.
    pub urls: usize,
    /// HTML pages read.
    pub pages: usize,
    pub rendered: usize,
    pub render_errors: usize,
    /// Responses with status 4xx/5xx or no response at all.
    pub errors: usize,
    pub blocked_by_robots: usize,
    pub elapsed: Duration,
}

/// Crawls from `config.start_url` into `store` and returns what it did.
pub async fn crawl(config: CrawlConfig, store: &mut Store) -> Result<CrawlSummary> {
    let started = Instant::now();
    let client = fetch::client(&fetch::ClientOptions {
        user_agent: &config.user_agent,
        timeout: config.timeout,
        concurrency: config.concurrency,
    })?;
    let crawl_id = store.start_crawl(config.start_url.as_str(), &config.to_json())?;
    let scope = Scope::new(&config.start_url, config.crawl_subdomains);
    let mut robots = robots::Robots::new(&config.robots_agent);
    robots.load(&client, &config.start_url).await;

    let render_threads = config.render.as_ref().map_or(0, |render| render.threads);
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        render: config.render.clone().map(RenderPool::new),
        http_slots: Semaphore::new(config.concurrency.max(1)),
        external_slots: Semaphore::new(config.concurrency.max(1)),
        external_timeout: config.timeout.min(EXTERNAL_TIMEOUT),
        max_body: config.max_body,
    });
    // Pages waiting for a render thread keep their HTTP slot free, so allow
    // a few of them per thread on top of the requests.
    let max_in_flight = config.concurrency.max(1) * 2 + render_threads * 2;

    let mut frontier = Frontier::new(config.max_urls, config.max_depth);
    frontier.push(config.start_url.clone(), 0, true, None, Edge::Other);

    let mut in_sitemap = HashSet::new();
    if config.use_sitemaps {
        let mut roots = robots.sitemaps(&config.start_url);
        if roots.is_empty() {
            roots.push(format!("{}/sitemap.xml", config.start_url.origin().ascii_serialization()));
        }
        in_sitemap = robots::sitemap_urls(&client, roots, config.max_urls).await;
        tracing::info!(urls = in_sitemap.len(), "sitemaps read");
        for listed in &in_sitemap {
            if let Ok(url) = Url::parse(listed) {
                if scope.internal(&url) {
                    frontier.push(url, 1, true, None, Edge::Other);
                }
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
        elapsed: Duration::ZERO,
    };
    let mut tasks = JoinSet::new();
    let mut last_progress = Instant::now();
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
                        frontier.known[item.index].id = Some(store.save_response(crawl_id, &http, true)?);
                        summary.urls += 1;
                        continue;
                    }
                }
            }
            tasks.spawn(process(ctx.clone(), item));
        }
        let Some(joined) = tasks.join_next().await else { break };
        let outcome = match joined {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!(%error, "crawl task failed");
                continue;
            }
        };
        record(store, crawl_id, &outcome, &mut frontier, &mut summary)?;
        discover(&outcome, &config, &scope, &mut frontier);
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

    let depths = frontier.depths();
    let states: Vec<_> = frontier
        .known
        .iter()
        .zip(depths)
        .filter_map(|(known, depth)| {
            let id = known.id?;
            Some((id, depth, in_sitemap.contains(&known.url), known.blocked))
        })
        .collect();
    store.set_crawl_states(&states)?;
    store.finish_crawl(crawl_id)?;
    summary.elapsed = started.elapsed();
    Ok(summary)
}

/// Longest wait for an external link's response headers.
const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(10);

struct Ctx {
    client: reqwest::Client,
    render: Option<RenderPool>,
    http_slots: Semaphore,
    external_slots: Semaphore,
    external_timeout: Duration,
    max_body: usize,
}

struct Item {
    url: Url,
    index: usize,
    depth: u32,
    /// Internal URLs are read and their HTML followed; external ones are
    /// requested for the status only.
    internal: bool,
}

struct Outcome {
    item: Item,
    http: HttpInfo,
    raw: Option<PageFacts>,
    rendered: Option<(Rendered, Option<PageFacts>)>,
}

async fn process(ctx: Arc<Ctx>, item: Item) -> Outcome {
    let (http, body) = {
        let (slots, timeout) = if item.internal {
            (&ctx.http_slots, None)
        } else {
            (&ctx.external_slots, Some(ctx.external_timeout))
        };
        let _slot = slots.acquire().await.expect("HTTP slots are never closed");
        fetch::fetch(&ctx.client, &item.url, item.internal, ctx.max_body, timeout).await
    };
    let raw = body.map(|html| seo_page::extract(&html, &item.url, Source::Raw));
    let rendered = match (&ctx.render, &raw) {
        (Some(pool), Some(_)) => {
            let mut rendered = pool.render(item.url.to_string()).await;
            let facts = rendered.error.is_none().then(|| {
                let base = Url::parse(&rendered.final_url).unwrap_or_else(|_| item.url.clone());
                seo_page::extract(&rendered.html, &base, Source::Rendered)
            });
            rendered.html = String::new();
            Some((rendered, facts))
        }
        _ => None,
    };
    Outcome { item, http, raw, rendered }
}

fn record(
    store: &mut Store,
    crawl_id: i64,
    outcome: &Outcome,
    frontier: &mut Frontier,
    summary: &mut CrawlSummary,
) -> Result<()> {
    let url_id = store.save_response(crawl_id, &outcome.http, outcome.item.internal)?;
    frontier.known[outcome.item.index].id = Some(url_id);
    summary.urls += 1;
    if outcome.http.status == 0 || outcome.http.status >= 400 {
        summary.errors += 1;
    }
    if let Some(raw) = &outcome.raw {
        summary.pages += 1;
        store.save_facts(crawl_id, url_id, raw)?;
    }
    if let Some((rendered, facts)) = &outcome.rendered {
        summary.rendered += 1;
        if rendered.error.is_some() {
            summary.render_errors += 1;
        }
        store.save_render(
            url_id,
            &RenderRow {
                final_url: &rendered.final_url,
                error: rendered.error.as_deref(),
                dcl_ms: rendered.dcl_ms,
                total_ms: rendered.total_ms,
                settled: rendered.settled,
                load_fired: rendered.load_fired,
                console_errors: &rendered.console_errors,
                resources_json: &serde_json::to_string(&rendered.resources)?,
            },
        )?;
        if let Some(facts) = facts {
            store.save_facts(crawl_id, url_id, facts)?;
        }
    }
    Ok(())
}

/// Queues what the outcome points at: the redirect target, then every URL
/// of the raw and rendered documents.
fn discover(outcome: &Outcome, config: &CrawlConfig, scope: &Scope, frontier: &mut Frontier) {
    let from = outcome.item.index;
    let depth = outcome.item.depth;
    let mut queue = |href: &str, depth: u32, edge: Edge| {
        let Ok(url) = Url::parse(href) else { return };
        let internal = scope.internal(&url);
        if edge == Edge::Resource && !config.check_resources {
            return;
        }
        if !internal && edge != Edge::Resource && !config.check_external {
            return;
        }
        frontier.push(url, depth, internal, Some(from), edge);
    };
    if outcome.http.is_redirect() {
        if let Some(location) = &outcome.http.location {
            queue(location, depth, Edge::Redirect);
        }
    }
    // Only internal documents are read, so their links are followed.
    let documents = [outcome.raw.as_ref(), outcome.rendered.as_ref().and_then(|(_, facts)| facts.as_ref())];
    for facts in documents.into_iter().flatten() {
        let next = depth + 1;
        for link in &facts.links {
            if link.href.is_empty() || (link.nofollow && !config.follow_nofollow) {
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
}

/// Which hosts are internal.
struct Scope {
    host: String,
    /// The start host without `www.`, for subdomain matching.
    base: String,
    subdomains: bool,
}

impl Scope {
    fn new(start: &Url, subdomains: bool) -> Self {
        let host = start.host_str().unwrap_or_default().to_ascii_lowercase();
        let base = host.strip_prefix("www.").unwrap_or(&host).to_string();
        Scope { host, base, subdomains }
    }

    /// Same host in either scheme, or a subdomain when the crawl covers them.
    fn internal(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else { return false };
        host == self.host
            || (self.subdomains
                && (host == self.base || host.strip_suffix(self.base.as_str()).is_some_and(|rest| rest.ends_with('.'))))
    }
}

/// How a URL was found from another, for click depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    id: Option<i64>,
    blocked: bool,
    /// (target, edge) for every click, redirect and resource from this URL.
    edges: Vec<(u32, Edge)>,
}

/// URLs known to the crawl in discovery order and the ones still to fetch.
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

    fn push(&mut self, mut url: Url, depth: u32, internal: bool, from: Option<usize>, edge: Edge) {
        url.set_fragment(None);
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
                self.known.push(Known { url: url.to_string(), id: None, blocked: false, edges: Vec::new() });
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

    /// Clicks from the start URL to each known URL: links cost one,
    /// redirects none, and a resource sits one below its page. `None` for
    /// URLs nothing reaches, such as pages found only in a sitemap.
    fn depths(&self) -> Vec<Option<u32>> {
        let mut depths: Vec<Option<u32>> = vec![None; self.known.len()];
        if self.known.is_empty() {
            return depths;
        }
        // 0-1 breadth-first search: redirects go to the front of the queue.
        depths[0] = Some(0);
        let mut queue = VecDeque::from([0usize]);
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
        let scope = Scope::new(&url("https://www.example.com/"), false);
        assert!(scope.internal(&url("http://www.example.com/a")));
        assert!(!scope.internal(&url("https://blog.example.com/")));
        let scope = Scope::new(&url("https://www.example.com/"), true);
        assert!(scope.internal(&url("https://blog.example.com/")));
        assert!(scope.internal(&url("https://example.com/")));
        assert!(!scope.internal(&url("https://notexample.com/")));
    }

    #[test]
    fn depths_follow_clicks_only() {
        let mut frontier = Frontier::new(100, None);
        frontier.push(url("https://e.com/"), 0, true, None, Edge::Other);
        frontier.push(url("https://e.com/a"), 1, true, Some(0), Edge::Click);
        frontier.push(url("https://e.com/b"), 2, true, Some(1), Edge::Click);
        frontier.push(url("https://e.com/b#x"), 1, true, Some(0), Edge::Click);
        frontier.push(url("https://e.com/canonical"), 1, true, Some(0), Edge::Other);
        frontier.push(url("https://e.com/orphan"), 1, true, None, Edge::Other);
        frontier.push(url("https://e.com/moved"), 2, true, Some(1), Edge::Redirect);
        frontier.push(url("https://e.com/img.png"), 2, true, Some(1), Edge::Resource);
        assert_eq!(frontier.known.len(), 7);
        assert_eq!(
            frontier.depths(),
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
}
