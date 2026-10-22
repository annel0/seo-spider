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

    /// The settings a crawl runs with. Following links needs them read: a
    /// crawl of whole documents extracts links even when `extract` leaves
    /// them out, and one that reads no `<body>` (nor renders) finds pages in
    /// the sitemaps.
    /// A rendering locale also becomes the crawl's Accept-Language unless a
    /// header sets one. Returns what was changed, for the log.
    pub fn resolve(&mut self) -> Vec<&'static str> {
        let mut changed = Vec::new();
        if let Some(locale) = self.render.as_ref().and_then(|render| render.locale.as_deref()) {
            if !self.http.headers.keys().any(|name| name.eq_ignore_ascii_case("accept-language")) {
                self.http.headers.insert("Accept-Language".into(), seo_render::accept_language(locale));
                changed.push("http.headers Accept-Language: from render.locale");
            }
        }
        if self.follow_links {
            match self.fetch {
                FetchLevel::Full if !self.extract.links => {
                    self.extract.links = true;
                    changed.push("extract.links: following links needs them extracted");
                }
                // A rendered DOM has the links the raw document was not read for.
                FetchLevel::Head | FetchLevel::Status if !self.use_sitemaps && self.render.is_none() => {
                    self.use_sitemaps = true;
                    changed.push("use_sitemaps: a crawl that reads no <body> finds pages in sitemaps");
                }
                _ => {}
            }
        }
        changed
    }

    /// A named starting point for the settings, from the least downloaded
    /// to the most:
    ///
    /// - `headers`: status and response headers only (HEAD requests, no
    ///   body), URLs from the start list and sitemaps;
    /// - `minimal`: response headers and `<head>` only, URLs from the start
    ///   list and sitemaps, nothing else requested;
    /// - `seo` (the default): the whole raw HTML, links followed, resources
    ///   and external links checked;
    /// - `js`: `seo` plus JavaScript rendering;
    /// - `dump`: `js` keeping the raw and rendered documents.
    pub fn preset(name: &str) -> Option<Self> {
        let mut config = CrawlConfig::default();
        match name {
            "headers" => {
                config.fetch = FetchLevel::Status;
                config.follow_links = false;
                config.use_sitemaps = true;
                config.check_external = false;
                config.check_resources = false;
                config.extract = ExtractOptions::head_only();
            }
            "minimal" => {
                config.fetch = FetchLevel::Head;
                config.http.http2 = true;
                config.follow_links = false;
                config.use_sitemaps = true;
                config.check_external = false;
                config.check_resources = false;
                config.extract = ExtractOptions::head_only();
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
    /// Requests in flight to the site.
    pub concurrency: usize,
    /// External URLs checked at once, spread over many hosts, with
    /// `external_timeout_ms`; no more than `external_per_host` on one host.
    pub external_concurrency: usize,
    pub external_per_host: usize,
    /// Upper bound on requests per second to the site; none by default.
    pub max_rps: Option<f64>,
    /// Extra attempts after no response, 429 or (for the site) 5xx, waiting
    /// `Retry-After` or a doubling backoff. External URLs get at most one.
    pub retries: u32,
    pub timeout_ms: u64,
    pub external_timeout_ms: u64,
    /// DNS, TCP and TLS together. Below the timeouts, so a host that does
    /// not answer at all is told apart from a slow response.
    pub connect_timeout_ms: u64,
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
    /// Offer HTTP/2 to servers. A `fetch = "head"` crawl gains most: over
    /// HTTP/1.1 each read cut at `</head>` costs the connection (vuejs.org:
    /// 4.5 s -> 1.7 s). Off by default: a whole-document crawl checks
    /// resources too, and some hosts answer a GET after a refused HEAD on
    /// the same HTTP/2 connection only at the timeout (Vercel-hosted images
    /// linked from vuejs.org: that crawl went from 2.4 s to 21 s).
    pub http2: bool,
    /// PEM files of root certificates trusted besides the built-in ones.
    /// Sites on a Russian Trusted Root CA certificate (tbank.ru) otherwise
    /// fail with UnknownIssuer, as in Chrome. Applies to the crawler's own
    /// requests, not the renderer's.
    pub ca_certs: Vec<std::path::PathBuf>,
    /// Accept any certificate, expired or for another name included.
    pub accept_invalid_certs: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        HttpConfig {
            user_agent: concat!("Mozilla/5.0 (compatible; seo-spider/", env!("CARGO_PKG_VERSION"), ")").into(),
            robots_agent: "seo-spider".into(),
            concurrency: 16,
            external_concurrency: 32,
            external_per_host: 4,
            max_rps: None,
            retries: 1,
            timeout_ms: 20_000,
            external_timeout_ms: 10_000,
            connect_timeout_ms: 5_000,
            max_body: 10 << 20,
            head_max_bytes: 256 << 10,
            headers: BTreeMap::new(),
            cookies: true,
            head_for_status: true,
            http2: false,
            ca_certs: Vec::new(),
            accept_invalid_certs: false,
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

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms)
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
    /// Also write every link as a row of the `links` table, for SQL over
    /// links. Checks and exports read links from the stored facts and do
    /// not need it; the rows cost a third of a fast crawl's time.
    pub links: bool,
    /// zstd level for kept documents; 0 stores them uncompressed.
    pub compression_level: i32,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions { raw_html: false, rendered_html: false, links: false, compression_level: 3 }
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
    /// Settle once the content (nodes, text, links) has been still this
    /// long, while animations still write styles; 0 (the default) waits for
    /// all DOM activity to stop. A spinner shown while data loads from
    /// another site also leaves the content still, so keep it well above
    /// such a load.
    pub settle_content_quiet_ms: u64,
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
    /// Layout time a page may spend before layout reads get its last
    /// layout; `None`: no limit.
    pub layout_budget_ms: Option<u64>,
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
    /// IANA zone the rendered pages see, such as "Europe/Moscow"; the
    /// host's when unset.
    pub timezone: Option<String>,
    /// The rendered pages' language, such as "ru-RU" (navigator.language,
    /// Accept-Language); also the crawl's Accept-Language unless a header
    /// sets one.
    pub locale: Option<String>,
    /// Render the document the crawler downloaded instead of requesting it
    /// again: one request less per rendered page, but the page is then the
    /// one served to the crawler's user agent, not the renderer's.
    pub reuse_documents: bool,
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
            settle_content_quiet_ms: render.settle.content_quiet.map_or(0, |still| still.as_millis() as u64),
            navigation_timeout_ms: render.navigation_timeout.as_millis() as u64,
            user_agent: None,
            third_party_timeout_ms: render.third_party_timeout.map(|limit| limit.as_millis() as u64),
            block_trackers: render.block_trackers,
            cache_static: render.cache_static,
            cache_mb: render.cache_mb,
            fetch_cache_ttl_ms: render.fetch_cache_ttl.map(|ttl| ttl.as_millis() as u64),
            layout_budget_ms: render.layout_budget.map(|budget| budget.as_millis() as u64),
            timezone: render.timezone,
            locale: render.locale,
            reuse_documents: render.reuse_documents,
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
                content_quiet: (self.settle_content_quiet_ms > 0)
                    .then(|| Duration::from_millis(self.settle_content_quiet_ms)),
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
            layout_budget: self.layout_budget_ms.map(Duration::from_millis),
            timezone: self.timezone.clone(),
            locale: self.locale.clone(),
            reuse_documents: self.reuse_documents,
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
    fn a_render_locale_sets_the_crawl_accept_language_unless_given() {
        let render = RenderSettings { locale: Some("ru-RU".into()), ..RenderSettings::default() };
        let mut config = CrawlConfig { render: Some(render.clone()), ..CrawlConfig::default() };
        config.resolve();
        assert_eq!(config.http.headers.get("Accept-Language").map(String::as_str), Some("ru-RU,ru;q=0.9"));
        let mut given = CrawlConfig { render: Some(render), ..CrawlConfig::default() };
        given.http.headers.insert("accept-language".into(), "de".into());
        given.resolve();
        assert_eq!(given.http.headers.len(), 1);
    }

    #[test]
    fn following_links_turns_on_what_it_needs() {
        let mut config = CrawlConfig { extract: ExtractOptions::head_only(), ..CrawlConfig::default() };
        assert_eq!(config.resolve().len(), 1);
        assert!(config.extract.links);
        let mut config = CrawlConfig { fetch: FetchLevel::Head, ..CrawlConfig::default() };
        config.resolve();
        assert!(config.use_sitemaps);
        let mut listed = CrawlConfig { fetch: FetchLevel::Head, follow_links: false, ..CrawlConfig::default() };
        assert!(listed.resolve().is_empty() && !listed.use_sitemaps);
        let mut rendered = CrawlConfig { fetch: FetchLevel::Head, render: Some(RenderSettings::default()), ..CrawlConfig::default() };
        assert!(rendered.resolve().is_empty());
    }

    #[test]
    fn presets_differ_where_documented() {
        let headers = CrawlConfig::preset("headers").unwrap();
        assert_eq!(headers.fetch, FetchLevel::Status);
        assert!(!headers.follow_links && headers.use_sitemaps && !headers.check_resources);
        let minimal = CrawlConfig::preset("minimal").unwrap();
        assert_eq!(minimal.fetch, FetchLevel::Head);
        assert!(!minimal.follow_links && minimal.use_sitemaps);
        let dump = CrawlConfig::preset("dump").unwrap();
        assert!(dump.render.is_some() && dump.store.raw_html && dump.store.rendered_html);
        assert!(CrawlConfig::preset("nope").is_none());
    }
}
