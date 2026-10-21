//! robots.txt rules per origin and the URLs listed in XML sitemaps.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use reqwest::Client;
use scraper::{Html, Selector};
use texting_robots::Robot;
use url::Url;

/// robots.txt of each origin the crawl entered, fetched on first use.
pub struct Robots {
    agent: String,
    by_origin: HashMap<String, Option<Robot>>,
}

impl Robots {
    /// `agent` is the product token robots.txt groups are matched against.
    pub fn new(agent: &str) -> Self {
        Robots { agent: agent.to_string(), by_origin: HashMap::new() }
    }

    /// Fetches the origin's robots.txt unless it is known already.
    pub async fn load(&mut self, client: &Client, url: &Url) {
        let origin = url.origin().ascii_serialization();
        if self.by_origin.contains_key(&origin) {
            return;
        }
        let robots_url = format!("{origin}/robots.txt");
        let robot = match get(client, &robots_url, 1 << 20).await {
            Ok((200..=299, body)) => match Robot::new(&self.agent, &body) {
                Ok(robot) => Some(robot),
                Err(error) => {
                    tracing::warn!(%robots_url, %error, "robots.txt does not parse; allowing all");
                    None
                }
            },
            // 4xx means no rules. 5xx and network errors would make Google
            // stop crawling; an audit is more useful crawling and reporting.
            Ok((status, _)) => {
                if status >= 500 {
                    tracing::warn!(%robots_url, status, "robots.txt unavailable; allowing all");
                }
                None
            }
            Err(error) => {
                tracing::warn!(%robots_url, %error, "robots.txt unreachable; allowing all");
                None
            }
        };
        self.by_origin.insert(origin, robot);
    }

    /// Whether robots.txt of the URL's origin allows it. Origins not loaded
    /// yet count as allowed.
    pub fn allowed(&self, url: &Url) -> bool {
        match self.by_origin.get(&url.origin().ascii_serialization()) {
            Some(Some(robot)) => robot.allowed(url.as_str()),
            _ => true,
        }
    }

    /// `Sitemap:` lines of the origin's robots.txt.
    pub fn sitemaps(&self, url: &Url) -> Vec<String> {
        match self.by_origin.get(&url.origin().ascii_serialization()) {
            Some(Some(robot)) => robot.sitemaps.clone(),
            _ => Vec::new(),
        }
    }
}

/// URLs of the sitemaps reachable from `roots`, following sitemap indexes, up
/// to `max_urls`.
pub async fn sitemap_urls(client: &Client, roots: Vec<String>, max_urls: usize) -> HashSet<String> {
    const MAX_SITEMAPS: usize = 200;
    let mut pending = roots;
    let mut fetched = HashSet::new();
    let mut urls = HashSet::new();
    while let Some(sitemap) = pending.pop() {
        if fetched.len() >= MAX_SITEMAPS || urls.len() >= max_urls || !fetched.insert(sitemap.clone()) {
            continue;
        }
        let body = match get(client, &sitemap, 50 << 20).await {
            Ok((200..=299, body)) => body,
            Ok((status, _)) => {
                tracing::info!(%sitemap, status, "sitemap not available");
                continue;
            }
            Err(error) => {
                tracing::info!(%sitemap, %error, "sitemap unreachable");
                continue;
            }
        };
        let (children, locs) = parse_sitemap(&body);
        pending.extend(children);
        urls.extend(locs.into_iter().take(max_urls.saturating_sub(urls.len())));
    }
    urls
}

/// (child sitemaps of an index, page URLs of a urlset).
fn parse_sitemap(body: &[u8]) -> (Vec<String>, Vec<String>) {
    static CHILD: std::sync::LazyLock<Selector> =
        std::sync::LazyLock::new(|| Selector::parse("sitemap > loc").unwrap());
    static PAGE: std::sync::LazyLock<Selector> =
        std::sync::LazyLock::new(|| Selector::parse("url > loc").unwrap());
    let mut text = String::new();
    if body.starts_with(&[0x1f, 0x8b]) {
        if flate2::read::GzDecoder::new(body).read_to_string(&mut text).is_err() {
            return (Vec::new(), Vec::new());
        }
    } else {
        text = String::from_utf8_lossy(body).into_owned();
    }
    // html5ever reads the XML well enough for <loc> text.
    let document = Html::parse_document(&text);
    let locs = |selector: &Selector| {
        document
            .select(selector)
            .map(|loc| loc.text().collect::<String>().trim().to_string())
            .filter(|loc| Url::parse(loc).is_ok())
            .collect::<Vec<_>>()
    };
    (locs(&CHILD), locs(&PAGE))
}

/// GET following up to five redirects: (final status, body up to `max`).
async fn get(client: &Client, url: &str, max: usize) -> anyhow::Result<(u16, Vec<u8>)> {
    let mut url = Url::parse(url)?;
    for _ in 0..5 {
        let mut response = client.get(url.clone()).send().await?;
        if response.status().is_redirection() {
            if let Some(next) = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .and_then(|location| url.join(location).ok())
            {
                url = next;
                continue;
            }
        }
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > max {
                anyhow::bail!("{url} is larger than {max} bytes");
            }
        }
        return Ok((status, body));
    }
    anyhow::bail!("too many redirects from {url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_index_and_urlset() {
        let index = br#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
            <sitemap><loc>https://example.com/s1.xml</loc></sitemap></sitemapindex>"#;
        assert_eq!(parse_sitemap(index), (vec!["https://example.com/s1.xml".to_string()], vec![]));
        let urlset = br#"<urlset><url><loc> https://example.com/a </loc>
            <image:image><image:loc>https://example.com/i.png</image:loc></image:image></url></urlset>"#;
        assert_eq!(parse_sitemap(urlset), (vec![], vec!["https://example.com/a".to_string()]));
    }

    #[test]
    fn matches_agent_groups() {
        let robot = Robot::new("seo-spider", b"User-agent: *\nDisallow: /private\n").unwrap();
        let mut robots = Robots::new("seo-spider");
        robots.by_origin.insert("https://example.com".into(), Some(robot));
        assert!(!robots.allowed(&Url::parse("https://example.com/private/x").unwrap()));
        assert!(robots.allowed(&Url::parse("https://example.com/public").unwrap()));
    }
}
