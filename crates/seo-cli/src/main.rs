//! seo-spider: crawl a site into SQLite, then export the tabs as CSV or XLSX.
//!
//! Settings come in layers: a preset (`--preset`, `seo` by default), then a
//! TOML file (`--config`) merged over it, then command-line flags.

mod export;
mod report;

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use seo_crawl::{CrawlConfig, ExtractOptions, FetchLevel, RenderSettings};
use seo_store::{BodyKind, Store};
use url::Url;

#[derive(Parser)]
#[command(version, about = "SEO crawler with an embedded JavaScript renderer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl a site (or a list of URLs) into the database.
    Crawl(Box<CrawlArgs>),
    /// Run the checks again over a stored crawl.
    Check(CheckArgs),
    /// Write the tabs of a crawl as CSV files or one XLSX workbook.
    Export(ExportArgs),
    /// Write the documents a crawl kept (`store.raw_html`, `store.rendered_html`) as files.
    Bodies(BodiesArgs),
    /// Summarise a crawl: statuses, render timings, console errors, failed requests.
    Report(CheckArgs),
    /// Pages whose rendered documents differ between two crawls.
    Compare(CompareArgs),
}

#[derive(clap::Args)]
struct CompareArgs {
    first: PathBuf,
    second: PathBuf,
}

#[derive(clap::Args)]
struct CheckArgs {
    #[arg(long, default_value = "crawl.sqlite")]
    db: PathBuf,
    /// Crawl id; the latest crawl by default.
    #[arg(long)]
    crawl: Option<i64>,
}

#[derive(clap::Args)]
struct CrawlArgs {
    /// Start URLs.
    urls: Vec<Url>,
    #[arg(long, default_value = "crawl.sqlite")]
    db: PathBuf,
    /// Starting settings: minimal, seo, js or dump.
    #[arg(long, default_value = "seo")]
    preset: String,
    /// TOML file merged over the preset.
    #[arg(long)]
    config: Option<PathBuf>,
    /// File with one start URL per line; implies --no-follow unless --follow.
    #[arg(long)]
    list: Option<PathBuf>,
    /// Print the settings the crawl would run with, as TOML, and exit.
    #[arg(long)]
    print_config: bool,

    /// How much of each internal response to read: status, head or full.
    #[arg(long, value_enum)]
    fetch: Option<Fetch>,
    /// Parts to read from whole documents: `all`, `head`, or a comma list of
    /// links, resources, headings, text, structured_data, forms, dom_stats.
    #[arg(long)]
    extract: Option<String>,
    /// Only fetch the start URLs (list mode).
    #[arg(long, conflicts_with = "follow")]
    no_follow: bool,
    /// Follow links even in list mode.
    #[arg(long)]
    follow: bool,
    /// Stop queueing new internal pages once this many are known (resources
    /// and external links each get a budget of the same size).
    #[arg(long)]
    max_urls: Option<usize>,
    /// Do not queue URLs more than this many steps from a start URL.
    #[arg(long)]
    max_depth: Option<u32>,
    /// Queue only internal URLs matching this regular expression (repeatable).
    #[arg(long)]
    include: Vec<String>,
    /// Never queue URLs matching this regular expression (repeatable).
    #[arg(long)]
    exclude: Vec<String>,
    /// Drop this query parameter from URLs; `utm_*` matches a prefix (repeatable).
    #[arg(long = "strip-param")]
    strip_params: Vec<String>,
    /// Treat subdomains of the start hosts as internal.
    #[arg(long)]
    subdomains: bool,
    #[arg(long)]
    follow_nofollow: bool,
    /// Crawl URLs robots.txt disallows (they are still marked).
    #[arg(long)]
    ignore_robots: bool,
    /// Do not request external links.
    #[arg(long)]
    no_external: bool,
    /// Do not request images, scripts, stylesheets and media.
    #[arg(long)]
    no_resources: bool,
    /// Also crawl the URLs listed in XML sitemaps.
    #[arg(long)]
    sitemaps: bool,

    /// HTTP requests in flight to the site (external checks get as many more).
    #[arg(long)]
    concurrency: Option<usize>,
    /// Upper bound on requests per second to the site.
    #[arg(long)]
    max_rps: Option<f64>,
    /// Extra attempts after no response, 429 or 5xx.
    #[arg(long)]
    retries: Option<u32>,
    /// Request timeout, seconds.
    #[arg(long)]
    timeout: Option<u64>,
    #[arg(long)]
    user_agent: Option<String>,
    /// Extra request header, `Name: value` (repeatable).
    #[arg(long = "header")]
    headers: Vec<String>,

    /// Render pages with JavaScript on this many threads (0: raw HTML only).
    #[arg(long)]
    render_threads: Option<usize>,
    /// Longest wait for the content to settle after DOMContentLoaded, ms.
    #[arg(long)]
    settle_budget_ms: Option<u64>,
    /// DOM quiet period that counts as settled, ms.
    #[arg(long)]
    settle_quiet_ms: Option<u64>,
    /// Stall limit for third-party requests while rendering, ms (0: obscura's own).
    #[arg(long)]
    third_party_timeout_ms: Option<u64>,
    /// Let rendered pages load tracker and ad hosts.
    #[arg(long)]
    no_block_trackers: bool,
    /// Download scripts and styles for every rendered page instead of once.
    #[arg(long)]
    no_render_cache: bool,
    /// Reuse fetch()/XHR responses across rendered pages for this long, ms (0: never).
    #[arg(long)]
    fetch_cache_ttl_ms: Option<u64>,
    /// Measure IntersectionObserver targets for real instead of treating
    /// every target as visible (slower: needs layout).
    #[arg(long)]
    exact_intersection: bool,
    /// Let rendered pages reach loopback and private addresses. On by itself
    /// when a start URL is such an address.
    #[arg(long)]
    allow_private_network: bool,

    /// Keep documents: raw, rendered or both.
    #[arg(long, value_enum)]
    store_html: Option<StoreHtml>,
    /// Do not store links as rows (keeps the database small).
    #[arg(long)]
    no_store_links: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Fetch {
    Status,
    Head,
    Full,
}

#[derive(Clone, Copy, ValueEnum)]
enum StoreHtml {
    Raw,
    Rendered,
    Both,
}

#[derive(clap::Args)]
struct ExportArgs {
    #[arg(long, default_value = "crawl.sqlite")]
    db: PathBuf,
    /// Crawl id; the latest crawl by default.
    #[arg(long)]
    crawl: Option<i64>,
    /// Output directory for CSV, or the .xlsx file.
    #[arg(long, short)]
    out: PathBuf,
    #[arg(long, value_enum, default_value_t = Format::Xlsx)]
    format: Format,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Csv,
    Xlsx,
}

#[derive(clap::Args)]
struct BodiesArgs {
    #[arg(long, default_value = "crawl.sqlite")]
    db: PathBuf,
    /// Crawl id; the latest crawl by default.
    #[arg(long)]
    crawl: Option<i64>,
    /// Output directory.
    #[arg(long, short)]
    out: PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // Page console errors are stored per render; logging them too
                // buries the progress lines.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("seo_crawl=info,seo_cli=info,obscura::console=off,warn")
                }),
        )
        .with_writer(std::io::stderr)
        // Render threads are named render-N; the name tells whose request a
        // network log line is.
        .with_thread_names(true)
        .init();
    match Cli::parse().command {
        Command::Crawl(args) => crawl(*args),
        Command::Check(args) => check(args),
        Command::Export(args) => export(args),
        Command::Bodies(args) => bodies(args),
        Command::Report(args) => {
            let store = Store::open(&args.db)?;
            let crawl_id = crawl_id(&store, args.crawl)?;
            report::report(&store, crawl_id)
        }
        Command::Compare(args) => {
            let first = Store::open(&args.first)?;
            let second = Store::open(&args.second)?;
            let (first_id, second_id) = (crawl_id(&first, None)?, crawl_id(&second, None)?);
            report::compare(&first, first_id, &second, second_id)
        }
    }
}

/// The preset, with the TOML file merged over it table by table.
fn base_config(args: &CrawlArgs) -> Result<CrawlConfig> {
    let preset = CrawlConfig::preset(&args.preset)
        .with_context(|| format!("unknown preset {:?} (minimal, seo, js, dump)", args.preset))?;
    let Some(path) = &args.config else { return Ok(preset) };
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let file: toml::Value = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut merged = toml::Value::try_from(&preset)?;
    merge(&mut merged, file);
    merged.try_into().with_context(|| format!("settings in {}", path.display()))
}

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

fn parse_extract(spec: &str) -> Result<ExtractOptions> {
    match spec {
        "all" => return Ok(ExtractOptions::default()),
        "head" => return Ok(ExtractOptions::head_only()),
        _ => {}
    }
    let mut options = ExtractOptions::head_only();
    for part in spec.split(',').map(str::trim).filter(|part| !part.is_empty()) {
        match part {
            "links" => options.links = true,
            "resources" => options.resources = true,
            "headings" => options.headings = true,
            "text" => options.text = true,
            "structured_data" => options.structured_data = true,
            "forms" => options.forms = true,
            "dom_stats" => options.dom_stats = true,
            _ => bail!("unknown extract part {part:?}"),
        }
    }
    Ok(options)
}

fn crawl_config(args: &CrawlArgs) -> Result<CrawlConfig> {
    let mut config = base_config(args)?;
    config.start_urls.extend(args.urls.iter().cloned());
    if let Some(path) = &args.list {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            config.start_urls.push(Url::parse(line).with_context(|| format!("URL in {}: {line}", path.display()))?);
        }
        config.follow_links = args.follow;
    }
    if args.no_follow {
        config.follow_links = false;
    }
    if args.follow {
        config.follow_links = true;
    }
    if let Some(fetch) = args.fetch {
        config.fetch = match fetch {
            Fetch::Status => FetchLevel::Status,
            Fetch::Head => FetchLevel::Head,
            Fetch::Full => FetchLevel::Full,
        };
    }
    if let Some(spec) = &args.extract {
        config.extract = parse_extract(spec)?;
    }
    if let Some(max_urls) = args.max_urls {
        config.max_urls = max_urls;
    }
    if args.max_depth.is_some() {
        config.max_depth = args.max_depth;
    }
    config.include.extend(args.include.iter().cloned());
    config.exclude.extend(args.exclude.iter().cloned());
    config.strip_params.extend(args.strip_params.iter().cloned());
    config.crawl_subdomains |= args.subdomains;
    config.follow_nofollow |= args.follow_nofollow;
    config.respect_robots &= !args.ignore_robots;
    config.check_external &= !args.no_external;
    config.check_resources &= !args.no_resources;
    config.use_sitemaps |= args.sitemaps;
    if let Some(concurrency) = args.concurrency {
        config.http.concurrency = concurrency;
    }
    if args.max_rps.is_some() {
        config.http.max_rps = args.max_rps;
    }
    if let Some(retries) = args.retries {
        config.http.retries = retries;
    }
    if let Some(timeout) = args.timeout {
        config.http.timeout_ms = timeout * 1000;
    }
    if let Some(user_agent) = &args.user_agent {
        config.http.user_agent = user_agent.clone();
    }
    for header in &args.headers {
        let (name, value) = header.split_once(':').with_context(|| format!("header {header:?} is not `Name: value`"))?;
        config.http.headers.insert(name.trim().to_string(), value.trim().to_string());
    }
    match args.render_threads {
        Some(0) => config.render = None,
        Some(threads) => config.render.get_or_insert_with(RenderSettings::default).threads = threads,
        None => {}
    }
    if let Some(render) = &mut config.render {
        if let Some(budget) = args.settle_budget_ms {
            render.settle_budget_ms = budget;
        }
        if let Some(quiet) = args.settle_quiet_ms {
            render.settle_quiet_ms = quiet;
        }
        match args.third_party_timeout_ms {
            Some(0) => render.third_party_timeout_ms = None,
            Some(limit) => render.third_party_timeout_ms = Some(limit),
            None => {}
        }
        render.block_trackers &= !args.no_block_trackers;
        render.cache_static &= !args.no_render_cache;
        render.intersection_full_viewport &= !args.exact_intersection;
        match args.fetch_cache_ttl_ms {
            Some(0) => render.fetch_cache_ttl_ms = None,
            Some(ttl) => render.fetch_cache_ttl_ms = Some(ttl),
            None => {}
        }
        // The renderer keeps obscura's browser user agent unless one is
        // given, so pages run the scripts a browser would get.
        if let Some(user_agent) = &args.user_agent {
            render.user_agent = Some(user_agent.clone());
        }
        render.allow_private_network |=
            args.allow_private_network || config.start_urls.iter().any(is_private_host);
    }
    match args.store_html {
        Some(StoreHtml::Raw) => config.store.raw_html = true,
        Some(StoreHtml::Rendered) => config.store.rendered_html = true,
        Some(StoreHtml::Both) => (config.store.raw_html, config.store.rendered_html) = (true, true),
        None => {}
    }
    config.store.links &= !args.no_store_links;
    Ok(config)
}

fn crawl(args: CrawlArgs) -> Result<()> {
    let config = crawl_config(&args)?;
    if args.print_config {
        std::io::stdout().write_all(toml::to_string_pretty(&config)?.as_bytes())?;
        return Ok(());
    }
    if config.start_urls.is_empty() {
        bail!("give a start URL, --list or start_urls in --config");
    }
    let store = Store::open(&args.db)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let (mut store, summary) = runtime.block_on(seo_crawl::crawl(config, store))?;
    eprintln!(
        "crawl {}: {} URLs, {} pages, {} rendered ({} render errors), {} errors, {} blocked by robots.txt, {:.1} MB read in {:.1} s ({:.0} URLs/s)",
        summary.crawl_id,
        summary.urls,
        summary.pages,
        summary.rendered,
        summary.render_errors,
        summary.errors,
        summary.blocked_by_robots,
        summary.bytes as f64 / 1048576.0,
        summary.elapsed.as_secs_f64(),
        summary.urls as f64 / summary.elapsed.as_secs_f64().max(0.001),
    );
    let checks_started = std::time::Instant::now();
    let issues = seo_crawl::run_checks(&mut store, summary.crawl_id)?;
    eprintln!("{issues} issues in {:.1} s", checks_started.elapsed().as_secs_f64());
    eprintln!("database: {}", args.db.display());
    // Render threads hold V8 isolates; exiting without tearing them down is
    // faster and safe once the store is closed.
    drop(store);
    runtime.shutdown_timeout(Duration::from_secs(1));
    std::process::exit(0);
}

/// localhost, loopback, private and link-local addresses.
fn is_private_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unique_local(),
        None => false,
    }
}

fn crawl_id(store: &Store, crawl: Option<i64>) -> Result<i64> {
    match crawl {
        Some(id) => Ok(id),
        None => store.last_crawl()?.context("the database has no crawls"),
    }
}

fn check(args: CheckArgs) -> Result<()> {
    let mut store = Store::open(&args.db)?;
    store.ensure_read_indexes()?;
    let crawl_id = crawl_id(&store, args.crawl)?;
    let started = std::time::Instant::now();
    let issues = seo_crawl::run_checks(&mut store, crawl_id)?;
    eprintln!("crawl {crawl_id}: {issues} issues in {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

fn export(args: ExportArgs) -> Result<()> {
    let store = Store::open(&args.db)?;
    store.ensure_read_indexes()?;
    let crawl_id = crawl_id(&store, args.crawl)?;
    let tables = export::tables(&store, crawl_id)?;
    match args.format {
        Format::Csv => export::write_csv(&tables, &args.out)?,
        Format::Xlsx => export::write_xlsx(&tables, &args.out)?,
    }
    for table in &tables {
        eprintln!("{}: {} rows", table.name, table.rows.len());
    }
    eprintln!("written: {}", args.out.display());
    Ok(())
}

fn bodies(args: BodiesArgs) -> Result<()> {
    let store = Store::open(&args.db)?;
    let crawl_id = crawl_id(&store, args.crawl)?;
    std::fs::create_dir_all(&args.out).with_context(|| format!("create {}", args.out.display()))?;
    let mut index = csv::Writer::from_path(args.out.join("index.csv"))?;
    index.write_record(["file", "kind", "url"])?;
    let mut written = 0usize;
    for url in store.urls(crawl_id)? {
        for kind in [BodyKind::Raw, BodyKind::Rendered] {
            let Some((encoding, data)) = store.body(url.id, kind)? else { continue };
            let html = match encoding.as_str() {
                "zstd" => zstd::stream::decode_all(data.as_slice())?,
                _ => data,
            };
            let file = format!("{}.{}.html", url.id, kind.as_str());
            std::fs::write(args.out.join(&file), html)?;
            index.write_record([file.as_str(), kind.as_str(), url.http.url.as_str()])?;
            written += 1;
        }
    }
    index.flush()?;
    eprintln!("{written} documents written to {}", args.out.display());
    Ok(())
}
