//! Spike: does an embedded obscura render pool give a crawler what it needs?
//!
//!   render-spike bench <urls-file> <threads>        render a URL list
//!   render-spike crawl <start-url> <max-pages> <threads>
//!                                                   spider crawls over HTTP,
//!                                                   HTML pages go to the pool
//!
//! Each render worker is its own OS thread with a current-thread runtime and a
//! single obscura Page, the way obscura's CDP server runs a connection. Pages
//! are !Send, so URLs and results cross threads over channels.

use std::sync::Arc;
use std::time::{Duration, Instant};

use obscura_browser::{BrowserContext, Page, WaitUntil};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;

/// What one render reads from the page, in a single evaluation.
const PROBE: &str = r#"(() => {
  const q = s => document.querySelector(s);
  let words = 0;
  if (document.body) {
    const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {
      acceptNode: n => /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE)$/.test(n.parentNode.nodeName)
        ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT });
    let n; while ((n = w.nextNode())) { const t = n.nodeValue.trim(); if (t) words += t.split(/\s+/).length; }
  }
  return {
    ready: document.readyState,
    title: document.title,
    canonical: (q('link[rel=canonical]') || {}).href || null,
    h1: document.querySelectorAll('h1').length,
    links: document.querySelectorAll('a[href]').length,
    words,
    html: document.documentElement.outerHTML.length,
  };
})()"#;

struct Job {
    url: String,
    reply: oneshot::Sender<Rendered>,
}

#[derive(Debug, Serialize)]
struct Rendered {
    url: String,
    worker: usize,
    error: Option<String>,
    status: Option<u16>,
    dcl_ms: u128,
    total_ms: u128,
    load_fired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    settled: Option<bool>,
    requests: usize,
    failed: usize,
    probe: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    request_log: Vec<String>,
}

async fn render(page: &mut Page, worker: usize, url: &str, load_budget: Duration) -> Rendered {
    let started = Instant::now();
    let mut out = Rendered {
        url: url.to_string(),
        worker,
        error: None,
        status: None,
        dcl_ms: 0,
        total_ms: 0,
        load_fired: false,
        settled: None,
        requests: 0,
        failed: 0,
        probe: Value::Null,
        request_log: Vec::new(),
    };
    let nav = tokio::time::timeout(
        Duration::from_secs(60),
        page.navigate_with_wait(url, WaitUntil::DomContentLoaded),
    )
    .await;
    out.dcl_ms = started.elapsed().as_millis();
    match nav {
        Err(_) => out.error = Some("navigation timed out".into()),
        Ok(Err(error)) => out.error = Some(error.to_string()),
        Ok(Ok(())) => {}
    }
    if out.error.is_none() {
        // SPIKE_WAIT=content: stop once the content is quiet instead of
        // waiting for the load event.
        if std::env::var("SPIKE_WAIT").as_deref() == Ok("content") {
            let ms = |key: &str, default: u64| {
                Duration::from_millis(
                    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default),
                )
            };
            let policy = obscura_browser::SettlePolicy {
                budget: load_budget,
                quiet: ms("SPIKE_QUIET_MS", 500),
                quiet_after_load: ms("SPIKE_QUIET_AFTER_LOAD_MS", 200),
                grace: ms("SPIKE_GRACE_MS", 1_000),
            };
            let settle = page.settle_content(policy).await;
            out.load_fired = settle.load_fired;
            out.settled = Some(settle.settled);
        } else {
            out.load_fired = page.advance_pending_load(load_budget).await;
        }
        out.probe = match page.evaluate_for_cdp_with_timeout(PROBE, true, false, 10_000).await {
            Ok(info) => info.value.unwrap_or(Value::Null),
            Err(error) => Value::String(error),
        };
    }
    out.total_ms = started.elapsed().as_millis();
    out.status = page
        .network_events
        .iter()
        .find(|event| event.resource_type == "Document")
        .map(|event| event.status);
    out.requests = page.network_events.len();
    out.failed = page
        .network_events
        .iter()
        .filter(|event| event.status == 0 || event.error_text.is_some())
        .count();
    if std::env::var_os("SPIKE_REQUESTS").is_some() {
        out.request_log = page
            .network_events
            .iter()
            .map(|event| {
                format!(
                    "{} {} {}{}",
                    event.status,
                    event.resource_type,
                    event.url,
                    event.error_text.as_deref().map(|e| format!(" ({e})")).unwrap_or_default()
                )
            })
            .collect();
    }
    out
}

fn new_context(worker: usize) -> Arc<BrowserContext> {
    Arc::new(BrowserContext::with_full_options(format!("w{worker}"), None, false, None))
}

fn new_page(worker: usize, context: &Arc<BrowserContext>) -> Page {
    Page::new(format!("page-{worker}"), context.clone())
}

/// One render worker: an OS thread owning its runtime and its Page.
fn spawn_worker(worker: usize, jobs: async_channel::Receiver<Job>, load_budget: Duration) {
    std::thread::Builder::new()
        .name(format!("render-{worker}"))
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("render runtime");
            let local = tokio::task::LocalSet::new();
            // SPIKE_FRESH=page: a new Page per URL on the same context;
            // SPIKE_FRESH=context: a new context too. Default: one Page reused.
            let fresh = std::env::var("SPIKE_FRESH").unwrap_or_default();
            local.block_on(&runtime, async move {
                let mut context = new_context(worker);
                let mut page = new_page(worker, &context);
                while let Ok(job) = jobs.recv().await {
                    if fresh == "context" {
                        context = new_context(worker);
                    }
                    if fresh == "page" || fresh == "context" {
                        page = new_page(worker, &context);
                    }
                    let rendered = render(&mut page, worker, &job.url, load_budget).await;
                    if rendered.error.as_deref() == Some("navigation timed out") {
                        // A dropped navigation can leave the page mid-load.
                        page = new_page(worker, &context);
                    }
                    let _ = job.reply.send(rendered);
                }
            });
        })
        .expect("spawn render thread");
}

struct Pool {
    jobs: async_channel::Sender<Job>,
}

impl Pool {
    fn new(threads: usize, load_budget: Duration) -> Self {
        let (tx, rx) = async_channel::unbounded();
        for worker in 0..threads {
            spawn_worker(worker, rx.clone(), load_budget);
        }
        Pool { jobs: tx }
    }

    fn submit(&self, url: String) -> oneshot::Receiver<Rendered> {
        let (reply, rx) = oneshot::channel();
        let _ = self.jobs.try_send(Job { url, reply });
        rx
    }
}

fn rss_mb(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find(|line| line.starts_with(field)).and_then(|line| {
                line.split_whitespace().nth(1).and_then(|kb| kb.parse::<u64>().ok())
            })
        })
        .map_or(0, |kb| kb / 1024)
}

fn summarize(results: &[Rendered], wall: Duration, threads: usize) {
    let ok = results.iter().filter(|r| r.error.is_none()).count();
    let mut dcl: Vec<u128> = results.iter().map(|r| r.dcl_ms).collect();
    let mut total: Vec<u128> = results.iter().map(|r| r.total_ms).collect();
    dcl.sort_unstable();
    total.sort_unstable();
    let median = |v: &[u128]| v.get(v.len() / 2).copied().unwrap_or(0);
    println!(
        "{}",
        serde_json::json!({
            "threads": threads,
            "pages": results.len(),
            "ok": ok,
            "load_fired": results.iter().filter(|r| r.load_fired).count(),
            "settled": results.iter().filter(|r| r.settled == Some(true)).count(),
            "wall_s": (wall.as_secs_f64() * 10.0).round() / 10.0,
            "pages_per_s": ((results.len() as f64 / wall.as_secs_f64()) * 100.0).round() / 100.0,
            "median_dcl_ms": median(&dcl),
            "median_total_ms": median(&total),
            "peak_rss_mb": rss_mb("VmHWM:"),
            "rss_mb": rss_mb("VmRSS:"),
        })
    );
}

async fn bench(path: &str, threads: usize, load_budget: Duration) -> anyhow::Result<()> {
    let urls: Vec<String> = std::fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    let pool = Pool::new(threads, load_budget);
    let started = Instant::now();
    let pending: Vec<_> = urls.into_iter().map(|url| pool.submit(url)).collect();
    let mut results = Vec::new();
    for rx in pending {
        if let Ok(rendered) = rx.await {
            if std::env::var_os("SPIKE_VERBOSE").is_some() {
                println!("{}", serde_json::to_string(&rendered)?);
            }
            results.push(rendered);
        }
    }
    summarize(&results, started.elapsed(), threads);
    Ok(())
}

async fn crawl(start: &str, max_pages: u32, threads: usize, load_budget: Duration) -> anyhow::Result<()> {
    let pool = Arc::new(Pool::new(threads, load_budget));
    let mut website = spider::website::Website::new(start);
    website
        .with_limit(max_pages)
        .with_respect_robots_txt(true)
        .with_concurrency_limit(Some(16));
    let mut pages = website.subscribe(1024);
    let started = Instant::now();
    let forward_pool = pool.clone();
    let collector = tokio::spawn(async move {
        let mut fetched = 0usize;
        let mut pending = Vec::new();
        while let Ok(page) = pages.recv().await {
            fetched += 1;
            let html = page
                .headers
                .as_ref()
                .and_then(|headers| headers.get("content-type"))
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("text/html"));
            if page.status_code.as_u16() == 200 && html {
                let raw_links = page.get_html().matches("<a ").count();
                pending.push((raw_links, forward_pool.submit(page.get_url().to_string())));
            }
        }
        (fetched, pending)
    });
    website.crawl().await;
    let http_done = started.elapsed();
    drop(website);
    let (fetched, pending) = collector.await?;
    let mut results = Vec::new();
    let mut fewer_links = 0;
    for (raw_links, rx) in pending {
        if let Ok(rendered) = rx.await {
            let rendered_links = rendered.probe.get("links").and_then(Value::as_u64).unwrap_or(0);
            if (rendered_links as usize) < raw_links / 2 {
                fewer_links += 1;
            }
            if std::env::var_os("SPIKE_VERBOSE").is_some() {
                println!("{}", serde_json::to_string(&rendered)?);
            }
            results.push(rendered);
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "http_pages": fetched,
            "http_crawl_s": (http_done.as_secs_f64() * 10.0).round() / 10.0,
            "rendered_with_half_the_raw_links_or_fewer": fewer_links,
        })
    );
    summarize(&results, started.elapsed(), threads);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    // The crawler reads the DOM, not pixels: skip obscura's image and font
    // preloading unless a run asks for it.
    for key in [
        "OBSCURA_RENDER_RESOURCE_WARMUP_MS",
        "OBSCURA_RENDER_RESOURCE_POST_SCRIPT_WARMUP_MS",
        "OBSCURA_RENDER_RESOURCE_SETTLE_WARMUP_MS",
    ] {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, "0");
        }
    }
    let load_budget = Duration::from_millis(
        std::env::var("SPIKE_LOAD_BUDGET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5_000),
    );
    let args: Vec<String> = std::env::args().collect();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    match args.get(1).map(String::as_str) {
        Some("bench") if args.len() == 4 => {
            runtime.block_on(bench(&args[2], args[3].parse()?, load_budget))
        }
        Some("crawl") if args.len() == 5 => runtime.block_on(crawl(
            &args[2],
            args[3].parse()?,
            args[4].parse()?,
            load_budget,
        )),
        _ => anyhow::bail!(
            "usage: render-spike bench <urls-file> <threads> | crawl <start-url> <max-pages> <threads>"
        ),
    }
}
