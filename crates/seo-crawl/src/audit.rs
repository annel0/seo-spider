//! Runs the check catalog over a finished crawl and stores the issues.

use anyhow::Result;
use seo_checks::{check_page, check_site, PageInput, SitePage};
use seo_store::Store;
use url::Url;

/// Checks every URL of the crawl, then the site as a whole, replacing the
/// crawl's stored issues. Returns the number of issues.
pub fn run_checks(store: &mut Store, crawl_id: i64) -> Result<usize> {
    let urls = store.urls(crawl_id)?;
    let mut issues: Vec<(i64, &'static str, Option<String>)> = Vec::new();
    let mut site = Vec::with_capacity(urls.len());
    let mut ids = Vec::with_capacity(urls.len());
    for stored in &urls {
        let Ok(url) = Url::parse(&stored.http.url) else { continue };
        let input = PageInput {
            url: &url,
            http: &stored.http,
            facts: stored.facts(),
            raw: stored.raw.as_ref(),
            rendered: stored.rendered.as_ref(),
            // Pages no click reaches have no depth; the orphan check covers them.
            depth: stored.depth.unwrap_or(0),
            in_sitemap: stored.in_sitemap,
            blocked_by_robots: stored.blocked_by_robots,
        };
        if stored.internal {
            issues.extend(check_page(&input).into_iter().map(|issue| (stored.id, issue.code, issue.detail)));
        }
        site.push(SitePage::from_input(&input).with_external(!stored.internal));
        ids.push(stored.id);
    }
    issues.extend(check_site(&site).into_iter().map(|(at, issue)| (ids[at], issue.code, issue.detail)));
    store.save_issues(crawl_id, &issues)?;
    Ok(issues.len())
}
