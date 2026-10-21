//! Runs the check catalog over a finished crawl and stores the issues.

use anyhow::Result;
use rayon::prelude::*;
use seo_checks::{check_page, check_site, Issue, PageInput, SitePage};
use seo_store::Store;
use url::Url;

/// URLs decoded and checked at a time; only their facts are in memory.
const CHUNK: usize = 2048;

/// Checks every URL of the crawl, then the site as a whole, replacing the
/// crawl's stored issues. Returns the number of issues.
///
/// Facts are read a chunk at a time and decoded and checked in parallel;
/// what the cross-page checks need (`SitePage`) is all that is kept.
pub fn run_checks(store: &mut Store, crawl_id: i64) -> Result<usize> {
    let mut issues: Vec<(i64, &'static str, Option<String>)> = Vec::new();
    let mut site = Vec::new();
    let mut ids = Vec::new();
    let started = std::time::Instant::now();
    store.for_each_url_chunk(crawl_id, CHUNK, |chunk| {
        let checked: Vec<Option<(i64, Vec<Issue>, SitePage)>> = chunk
            .into_par_iter()
            .map(|encoded| -> Result<_> {
                let stored = encoded.decode()?;
                let Ok(url) = Url::parse(&stored.http.url) else { return Ok(None) };
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
                let page_issues = if stored.internal { check_page(&input) } else { Vec::new() };
                let page = SitePage::from_input(&input).with_external(!stored.internal);
                Ok(Some((stored.id, page_issues, page)))
            })
            .collect::<Result<_>>()?;
        for (id, page_issues, page) in checked.into_iter().flatten() {
            issues.extend(page_issues.into_iter().map(|issue| (id, issue.code, issue.detail)));
            site.push(page);
            ids.push(id);
        }
        Ok(())
    })?;
    let pages_done = started.elapsed();
    issues.extend(check_site(&site).into_iter().map(|(at, issue)| (ids[at], issue.code, issue.detail)));
    let site_done = started.elapsed();
    store.save_issues(crawl_id, &issues)?;
    tracing::info!(
        pages_s = pages_done.as_secs_f64(),
        site_s = (site_done - pages_done).as_secs_f64(),
        save_s = (started.elapsed() - site_done).as_secs_f64(),
        "checks"
    );
    Ok(issues.len())
}
