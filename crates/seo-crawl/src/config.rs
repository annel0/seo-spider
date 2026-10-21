//! Everything a crawl can be told, in one serde structure: a TOML file, a
//! preset and command-line flags all end up here.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use seo_page::ExtractOptions;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CrawlConfig {
    /// Where the crawl starts. With `follow_links` off these are the only
    /// pages fetched (list mode), plus redirect targets and status checks.
    pub start_urls: Vec<Url>,
    /// Queue the links, canonicals, hreflang and pagination URLs pages point at.
    pub follow_links: bool,
    /// Stop queueing new internal pages once this many are known. Internal
    /// resources (images, scripts, styles, media) and external URLs each
    /// have a budget of the same size, and resources are fetched after the
    /// pages waiting, so they never take a page's place.
    pub max_urls: usize,
    /// URLs more than this many steps from a start URL are not queued.
    pub max_depth: Option<u32>,
    /// Regular expressions; when any is given an internal URL is queued only
    /// if it matches one. Start URLs are always fetched.
    pub include: Vec<String>,
    /// Regular expressions; a URL matching any is not queued.
    pub exclude: Vec<String>,
    /// Query parameters dropped from every URL before it is queued
    /// (`utm_source`, ...); a trailing `*` matches a prefix (`utm_*`).
    pub strip_params: Vec<String>,
    /// Subdomains of the start host count as internal.
    pub crawl_subdomains: bool,
    pub follow_nofollow: bool,
    /// Request external links once to report their status.
    pub check_external: bool,
    /// Request images, scripts, stylesheets and media to report their status.
    pub check_resources: bool,
    /// Queue the URLs of the sitemaps listed in robots.txt (or /sitemap.xml).
    pub use_sitemaps: bool,
    /// Skip URLs robots.txt disallows (they are still reported), and wait
    /// its Crawl-delay between requests when that is slower than max_rps.
    pub respect_robots: bool,
    pub http: HttpConfig,
    /// How much of each internal response is read.
    pub fetch: FetchLevel,
    /// What is read from the documents that are fetched whole.
    pub extract: ExtractOptions,
    pub store: StoreOptions,
    /// Render HTML pages with JavaScript; absent reads the raw HTML only.
    pub render: Option<RenderSettings>,
}

impl Default for CrawlConfig {
    fn default() -> Self {
        CrawlConfig {
            start_urls: Vec::new(),
            follow_links: true,
            max_urls: 10_000,
            max_depth: None,
            include: Vec::new(),
            // Cloudflare's email obfuscation: the link only becomes a
            // mailto: once its script runs, and the placeholder is a 404.
            exclude: vec!["/cdn-cgi/l/email-protection".into()],
            strip_params: Vec::new(),
            crawl_subdomains: false,
            follow_nofollow: false,
            check_external: true,
            check_resources: true,
            use_sitemaps: false,
            respect_robots: true,
            http: HttpConfig::default(),
            fetch: FetchLevel::Full,
            extract: ExtractOptions::default(),
            store: StoreOptions::default(),
            render: None,
        }
    }
}

impl CrawlConfig {
    pub fn new(start_url: Url) -> Self {
        CrawlConfig { start_urls: vec![start_url], ..CrawlConfig::default() }
    }

    /// A named starting point for the settings:
    ///
    /// - `minimal`: response headers and `<head>` only, URLs from the start
    ///   list and sitemaps, nothing else requested;
    /// - `seo` (the default): the whole raw HTML, links followed, resources
    ///   and external links checked;
    /// - `js`: `seo` plus JavaScript rendering;
    /// - `dump`: `js` keeping the raw and rendered documents.
    pub fn preset(name: &str) -> Option<Self> {
        let mut config = CrawlConfig::default();
        match name {
            "minimal" => {
                config.fetch = FetchLevel::Head;
                config.follow_links = false;
                config.use_sitemaps = true;
                config.check_external = false;
                config.check_resources = false;
                config.extract = ExtractOptions::head_only();
                config.store.links = false;
            }
            "seo" => {}
            "js" => config.render = Some(RenderSettings::default()),
            "dump" => {
                config.render = Some(RenderSettings::default());
                config.store.raw_html = true;
                config.store.rendered_html = true;
            }
            _ => return None,
        }
        Some(config)
    }
}

/// How much of each internal response is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchLevel {
    /// Status and headers only; the body is never read.
    Status,
    /// The document up to `</head>` (bounded by `http.head_max_bytes`):
    /// title, meta, canonical, hreflang. No links, so nothing is discovered
    /// beyond redirects, sitemaps and the start list.
    Head,
    /// The whole document.
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub user_agent: String,
    /// Product token robots.txt groups are matched against.
    pub robots_agent: String,
    /// Requests in flight to the site. External checks get as many slots of
    /// their own (spread over many hosts) and `external_timeout_ms`.
    pub concurrency: usize,
    /// Upper bound on requests per second to the site; none by default.
    pub max_rps: Option<f64>,
    /// Extra attempts after no response, 429 or (for the site) 5xx, waiting
    /// `Retry-After` or a doubling backoff. External URLs get at most one.
    pub retries: u32,
    pub timeout_ms: u64,
    pub external_timeout_ms: u64,
    /// Whole bodies are cut at this size.
    pub max_body: usize,
    /// How far into a document `fetch = "head"` reads looking for `</head>`.
    pub head_max_bytes: usize,
    /// Extra request headers (cookies, authorization, Accept-Language...).
    pub headers: BTreeMap<String, String>,
    /// Keep cookies the site sets for the rest of the crawl.
    pub cookies: bool,
    /// Check resources and external links with HEAD (GET when refused).
    pub head_for_status: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        HttpConfig {
            user_agent: concat!("Mozilla/5.0 (compatible; seo-spider/", env!("CARGO_PKG_VERSION"), ")").into(),
            robots_agent: "seo-spider".into(),
            concurrency: 16,
            max_rps: None,
            retries: 1,
            timeout_ms: 20_000,
            external_timeout_ms: 10_000,
            max_body: 10 << 20,
            head_max_bytes: 256 << 10,
            headers: BTreeMap::new(),
            cookies: true,
            head_for_status: true,
        }
    }
}

impl HttpConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub fn external_timeout(&self) -> Duration {
        Duration::from_millis(self.external_timeout_ms)
    }
}

/// What the crawl keeps besides responses and facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreOptions {
    /// Keep each internal HTML response body.
    pub raw_html: bool,
    /// Keep each rendered DOM.
    pub rendered_html: bool,
    /// Store every link as a row (inlinks, anchors); off keeps only the
    /// count per page.
    pub links: bool,
    /// zstd level for kept documents; 0 stores them uncompressed.
    pub compression_level: i32,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions { raw_html: false, rendered_html: false, links: true, compression_level: 3 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RenderSettings {
    pub threads: usize,
    /// Longest wait for the content to settle after DOMContentLoaded.
    pub settle_budget_ms: u64,
    /// DOM quiet period that counts as settled before `load`.
    pub settle_quiet_ms: u64,
    /// The same after `load`.
    pub settle_quiet_after_load_ms: u64,
    /// How long third-party requests in flight hold the snapshot.
    pub settle_grace_ms: u64,
    pub navigation_timeout_ms: u64,
    /// The renderer keeps obscura's browser user agent unless one is given.
    pub user_agent: Option<String>,
    /// Stall limit for requests to other sites; `None` keeps obscura's 10 s.
    pub third_party_timeout_ms: Option<u64>,
    pub block_trackers: bool,
    /// One cache of scripts, styles and fonts for all render threads for the
    /// whole crawl (`no-store` is honoured).
    pub cache_static: bool,
    pub cache_mb: usize,
    /// Reuse GET fetch()/XHR responses across pages for this long; `None`
    /// fetches every time.
    pub fetch_cache_ttl_ms: Option<u64>,
    /// IntersectionObserver sees every target in the viewport (lazy content
    /// loads, no layout to measure targets).
    pub intersection_full_viewport: bool,
    /// Layout downloads no images or web fonts.
    pub skip_images: bool,
    pub skip_fonts: bool,
    /// Let pages reach loopback and private addresses.
    pub allow_private_network: bool,
    /// Replace a render thread's page after this many renders.
    pub recycle_after: usize,
}

impl Default for RenderSettings {
    fn default() -> Self {
        let render = seo_render::RenderConfig::default();
        RenderSettings {
            threads: render.threads,
            settle_budget_ms: render.settle.budget.as_millis() as u64,
            settle_quiet_ms: render.settle.quiet.as_millis() as u64,
            settle_quiet_after_load_ms: render.settle.quiet_after_load.as_millis() as u64,
            settle_grace_ms: render.settle.grace.as_millis() as u64,
            navigation_timeout_ms: render.navigation_timeout.as_millis() as u64,
            user_agent: None,
            third_party_timeout_ms: render.third_party_timeout.map(|limit| limit.as_millis() as u64),
            block_trackers: render.block_trackers,
            cache_static: render.cache_static,
            cache_mb: render.cache_mb,
            fetch_cache_ttl_ms: render.fetch_cache_ttl.map(|ttl| ttl.as_millis() as u64),
            intersection_full_viewport: render.intersection_full_viewport,
            skip_images: render.skip_images,
            skip_fonts: render.skip_fonts,
            allow_private_network: false,
            recycle_after: render.recycle_after,
        }
    }
}

impl RenderSettings {
    pub fn to_render_config(&self) -> seo_render::RenderConfig {
        seo_render::RenderConfig {
            threads: self.threads.max(1),
            settle: seo_render::SettlePolicy {
                budget: Duration::from_millis(self.settle_budget_ms),
                quiet: Duration::from_millis(self.settle_quiet_ms),
                quiet_after_load: Duration::from_millis(self.settle_quiet_after_load_ms),
                grace: Duration::from_millis(self.settle_grace_ms),
            },
            navigation_timeout: Duration::from_millis(self.navigation_timeout_ms),
            user_agent: self.user_agent.clone(),
            recycle_after: self.recycle_after.max(1),
            allow_private_network: self.allow_private_network,
            third_party_timeout: self.third_party_timeout_ms.map(Duration::from_millis),
            block_trackers: self.block_trackers,
            cache_static: self.cache_static,
            cache_mb: self.cache_mb,
            fetch_cache_ttl: self.fetch_cache_ttl_ms.map(Duration::from_millis),
            intersection_full_viewport: self.intersection_full_viewport,
            skip_images: self.skip_images,
            skip_fonts: self.skip_fonts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_partial_toml_file() {
        let config: CrawlConfig = toml::from_str(
            r#"
            start_urls = ["https://example.com/"]
            fetch = "head"
            exclude = ["/tag/"]
            [http]
            concurrency = 4
            headers = { cookie = "a=b" }
            [extract]
            text = false
            [render]
            threads = 2
            "#,
        )
        .unwrap();
        assert_eq!(config.fetch, FetchLevel::Head);
        assert_eq!(config.http.concurrency, 4);
        assert_eq!(config.http.timeout_ms, 20_000);
        assert_eq!(config.http.headers["cookie"], "a=b");
        assert!(!config.extract.text && config.extract.links);
        assert_eq!(config.render.unwrap().threads, 2);
        assert!(toml::from_str::<CrawlConfig>("fetch = \"everything\"").is_err());
        assert!(toml::from_str::<CrawlConfig>("max_url = 5").is_err());
    }

    #[test]
    fn presets_differ_where_documented() {
        let minimal = CrawlConfig::preset("minimal").unwrap();
        assert_eq!(minimal.fetch, FetchLevel::Head);
        assert!(!minimal.follow_links && minimal.use_sitemaps);
        let dump = CrawlConfig::preset("dump").unwrap();
        assert!(dump.render.is_some() && dump.store.raw_html && dump.store.rendered_html);
        assert!(CrawlConfig::preset("nope").is_none());
    }
}
