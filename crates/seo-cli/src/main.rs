//! seo-spider: crawl a site into SQLite, then export the tabs as CSV or XLSX.

mod export;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use seo_crawl::{CrawlConfig, RenderConfig, SettlePolicy};
use seo_store::Store;
use url::Url;

#[derive(Parser)]
#[command(version, about = "SEO crawler with an embedded JavaScript renderer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl a site into the database.
    Crawl(CrawlArgs),
    /// Run the checks again over a stored crawl.
    Check(CheckArgs),
    /// Write the tabs of a crawl as CSV files or one XLSX workbook.
    Export(ExportArgs),
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
    /// Start URL.
    url: Url,
    #[arg(long, default_value = "crawl.sqlite")]
    db: PathBuf,
    /// Stop queueing new internal URLs once this many are known (external
    /// links get a budget of the same size).
    #[arg(long, default_value_t = 10_000)]
    max_urls: usize,
    /// Do not queue URLs more than this many steps from the start.
    #[arg(long)]
    max_depth: Option<u32>,
    /// HTTP requests in flight to the site (external checks get as many
    /// on top, with a 10 s timeout).
    #[arg(long, default_value_t = 16)]
    concurrency: usize,
    /// Render pages with JavaScript on this many threads (0: raw HTML only).
    #[arg(long, default_value_t = 0)]
    render_threads: usize,
    /// Longest wait for the content to settle after DOMContentLoaded, ms.
    #[arg(long, default_value_t = 5000)]
    settle_budget_ms: u64,
    /// DOM quiet period that counts as settled, ms.
    #[arg(long, default_value_t = 500)]
    settle_quiet_ms: u64,
    #[arg(long)]
    user_agent: Option<String>,
    /// Crawl URLs robots.txt disallows (they are still marked).
    #[arg(long)]
    ignore_robots: bool,
    #[arg(long)]
    follow_nofollow: bool,
    /// Treat subdomains of the start host as internal.
    #[arg(long)]
    subdomains: bool,
    /// Do not request external links.
    #[arg(long)]
    no_external: bool,
    /// Do not request images, scripts, stylesheets and media.
    #[arg(long)]
    no_resources: bool,
    /// Also crawl the URLs listed in XML sitemaps.
    #[arg(long)]
    sitemaps: bool,
    /// Request timeout, seconds.
    #[arg(long, default_value_t = 20)]
    timeout: u64,
    /// Let rendered pages reach loopback and private addresses. On by
    /// itself when the start URL is such an address.
    #[arg(long)]
    allow_private_network: bool,
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
        .init();
    match Cli::parse().command {
        Command::Crawl(args) => crawl(args),
        Command::Check(args) => check(args),
        Command::Export(args) => export(args),
    }
}

fn crawl(args: CrawlArgs) -> Result<()> {
    let allow_private_network = args.allow_private_network || is_private_host(&args.url);
    let mut config = CrawlConfig::new(args.url);
    config.max_urls = args.max_urls;
    config.max_depth = args.max_depth;
    config.concurrency = args.concurrency;
    config.respect_robots = !args.ignore_robots;
    config.follow_nofollow = args.follow_nofollow;
    config.crawl_subdomains = args.subdomains;
    config.check_external = !args.no_external;
    config.check_resources = !args.no_resources;
    config.use_sitemaps = args.sitemaps;
    config.timeout = Duration::from_secs(args.timeout);
    if let Some(user_agent) = &args.user_agent {
        config.user_agent = user_agent.clone();
    }
    if args.render_threads > 0 {
        config.render = Some(RenderConfig {
            threads: args.render_threads,
            settle: SettlePolicy {
                budget: Duration::from_millis(args.settle_budget_ms),
                quiet: Duration::from_millis(args.settle_quiet_ms),
                ..SettlePolicy::default()
            },
            // The renderer keeps obscura's browser user agent unless one is
            // given, so pages run the scripts a browser would get.
            user_agent: args.user_agent.clone(),
            allow_private_network,
            ..RenderConfig::default()
        });
    }
    let mut store = Store::open(&args.db)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let summary = runtime.block_on(seo_crawl::crawl(config, &mut store))?;
    eprintln!(
        "crawl {}: {} URLs, {} pages, {} rendered ({} render errors), {} errors, {} blocked by robots.txt in {:.1} s",
        summary.crawl_id,
        summary.urls,
        summary.pages,
        summary.rendered,
        summary.render_errors,
        summary.errors,
        summary.blocked_by_robots,
        summary.elapsed.as_secs_f64(),
    );
    let issues = seo_crawl::run_checks(&mut store, summary.crawl_id)?;
    eprintln!("{issues} issues");
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
    let crawl_id = crawl_id(&store, args.crawl)?;
    let issues = seo_crawl::run_checks(&mut store, crawl_id)?;
    eprintln!("crawl {crawl_id}: {issues} issues");
    Ok(())
}

fn export(args: ExportArgs) -> Result<()> {
    let store = Store::open(&args.db)?;
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
