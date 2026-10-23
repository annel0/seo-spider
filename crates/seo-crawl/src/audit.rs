//! Runs the check catalog over a finished crawl and stores the issues, and
//! the link graph between its URLs that the checks read.

use std::hash::BuildHasher;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rayon::prelude::*;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use seo_checks::{check_page, check_site_with, Issue, PageInput, SiteOptions, SitePage};
use seo_page::ExtractOptions;
use seo_store::{Source, Store};
use url::Url;

/// URLs decoded and checked at a time; only their facts are in memory.
const CHUNK: usize = 2048;

/// Whether a check's finding means anything given what was extracted: a
/// crawl that read only `<head>` has no links, headings or text, and their
/// checks would flag every page.
pub(crate) fn meaningful(code: &str, extracted: &ExtractOptions) -> bool {
    match code {
        "too_many_links" | "internal_nofollow_links" | "external_follow_links" | "links_to_http" | "dead_end"
        | "localhost_links" | "link_with_credentials" | "nofollow_to_indexable" | "incoming_follow_and_nofollow"
        | "internal_link_to_redirect" | "internal_link_to_error" | "orphan_page" | "external_link_redirect"
        | "external_link_broken" | "pagination_link_missing" | "js_links_only_in_rendered" => extracted.links,
        "h1_missing" | "headings_order_invalid" | "h1_duplicate" | "js_h1_only_in_rendered" | "js_h1_changed" => {
            extracted.headings
        }
        "little_content" | "duplicate_content" | "js_content_only_in_rendered" => extracted.text,
        "near_duplicate_content" => extracted.similarity,
        "duplicate_ids" | "dom_size" => extracted.dom_stats,
        "image_alt_missing" | "image_alt_too_long" | "image_too_large" | "picture_missing_img"
        | "image_without_size" => extracted.resources,
        "form_on_http" | "insecure_form" => extracted.forms,
        "structured_data_parse_error" | "schema_type_unknown" | "schema_property_invalid"
        | "rich_result_required_missing" | "rich_result_recommended_missing" => extracted.structured_data,
        _ => true,
    }
}

/// One copy of each link target for the site checks, shared by every link
/// to it. Sharded so the parallel decoding seldom waits on a lock.
struct Targets {
    shards: Vec<Mutex<FxHashSet<Arc<str>>>>,
}

impl Targets {
    fn new() -> Self {
        Targets { shards: (0..64).map(|_| Mutex::default()).collect() }
    }

    fn share(&self, target: &Arc<str>) -> Arc<str> {
        let shard = FxBuildHasher.hash_one(&**target) as usize % self.shards.len();
        let mut known = self.shards[shard].lock().expect("targets lock");
        match known.get(&**target) {
            Some(shared) => shared.clone(),
            None => {
                known.insert(target.clone());
                target.clone()
            }
        }
    }
}

/// Checks every URL of the crawl, then the site as a whole, replacing the
/// crawl's stored issues and link graph. Returns the number of issues.
///
/// Facts are read a chunk at a time and decoded and checked in parallel;
/// what the cross-page checks need (`SitePage`) is all that is kept.
pub fn run_checks(store: &mut Store, crawl_id: i64) -> Result<usize> {
    let mut issues: Vec<(i64, &'static str, Option<String>)> = Vec::new();
    let mut site = Vec::new();
    let mut ids = Vec::new();
    // What each page's facts were read with, for `meaningful`.
    let mut extracted: FxHashMap<i64, ExtractOptions> = FxHashMap::default();
    let targets = Targets::new();
    let started = std::time::Instant::now();
    store.for_each_url_chunk(crawl_id, CHUNK, |chunk| {
        type Checked = (i64, Vec<Issue>, SitePage, Option<ExtractOptions>);
        let checked: Vec<Option<Checked>> = chunk
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
                    render: stored.render.as_ref(),
                    // Pages no click reaches have no depth; the orphan check covers them.
                    depth: stored.depth.unwrap_or(0),
                    in_sitemap: stored.in_sitemap,
                    blocked_by_robots: stored.blocked_by_robots,
                };
                let page_issues = if stored.internal { check_page(&input) } else { Vec::new() };
                let mut page = SitePage::from_input(&input).with_external(!stored.internal);
                for link in &mut page.links {
                    link.target = targets.share(&link.target);
                }
                Ok(Some((stored.id, page_issues, page, stored.facts().map(|facts| facts.extracted))))
            })
            .collect::<Result<_>>()?;
        for (id, page_issues, page, read) in checked.into_iter().flatten() {
            if let Some(read) = read {
                extracted.insert(id, read);
            }
            issues.extend(page_issues.into_iter().map(|issue| (id, issue.code, issue.detail)));
            site.push(page);
            ids.push(id);
        }
        Ok(())
    })?;
    let pages_done = started.elapsed();
    // The crawl's own settings, so a later `check` judges it the same way.
    let settings = store.crawl_config(crawl_id)?;
    let config: serde_json::Value = serde_json::from_str(&settings).unwrap_or_default();
    let options = SiteOptions {
        near_duplicate_threshold: config["checks"]["near_duplicate_threshold"]
            .as_f64()
            .unwrap_or(SiteOptions::default().near_duplicate_threshold),
    };
    issues.extend(check_site_with(&site, &options).into_iter().map(|(at, issue)| (ids[at], issue.code, issue.detail)));
    let site_done = started.elapsed();
    // A URL that was not read (a status-only crawl, a redirect) has no
    // extraction of its own; whether its link checks mean anything depends
    // on whether the crawl read links anywhere: with none, every URL is an
    // orphan.
    let links_read = extracted.values().any(|read| read.links);
    let unread = ExtractOptions { links: links_read, ..ExtractOptions::default() };
    issues.retain(|(id, code, _)| meaningful(code, extracted.get(id).unwrap_or(&unread)));
    store.save_issues(crawl_id, &issues)?;
    let saved = started.elapsed();
    let targets = crate::LinkTargets::new(&serde_json::from_str(&settings).unwrap_or_default())?;
    let mut graph = inlinks(&site, &ids, &targets);
    let internal: Vec<bool> = site.iter().map(|page| !page.external).collect();
    drop(site);
    let edges: usize = graph.iter().map(|(_, sources)| sources.len()).sum();
    store.save_link_scores(crawl_id, &crate::rank::link_scores(&graph, &internal))?;
    store.save_inlinks(crawl_id, &mut graph)?;
    tracing::info!(
        pages_s = pages_done.as_secs_f64(),
        site_s = (site_done - pages_done).as_secs_f64(),
        save_s = (saved - site_done).as_secs_f64(),
        edges,
        edges_s = (started.elapsed() - saved).as_secs_f64(),
        "checks"
    );
    Ok(issues.len())
}

/// The link graph, by target: each page's links, and its redirect or meta
/// refresh, to the crawl's URLs, one source per page and target. A link is
/// matched as the crawler queued it (no fragment, stripped parameters
/// gone); links to URLs the crawl did not request, and to the page itself,
/// are left out. Pages are read a chunk at a time in parallel and their
/// links filed by target at once: no list of every link is ever whole.
fn inlinks(site: &[SitePage], ids: &[i64], targets: &crate::LinkTargets) -> Vec<(i64, Vec<Source>)> {
    let index: FxHashMap<&str, u32> = site.iter().enumerate().map(|(at, page)| (page.url.as_str(), at as u32)).collect();
    let find = |href: &str| -> Option<u32> {
        index.get(href).copied().or_else(|| targets.target(href).and_then(|url| index.get(url.as_str()).copied()))
    };
    let mut graph: Vec<(i64, Vec<Source>)> = ids.iter().map(|&id| (id, Vec::new())).collect();
    for (chunk, pages) in site.chunks(CHUNK).enumerate() {
        let found: Vec<Vec<(u32, Source)>> = pages
            .par_iter()
            .enumerate()
            .map(|(offset, page)| {
                let from = (chunk * CHUNK + offset) as u32;
                let from_id = ids[from as usize];
                let mut links: Vec<(u32, Source)> = Vec::new();
                let mut at: FxHashMap<u32, usize> = FxHashMap::default();
                for link in &page.links {
                    let Some(to) = find(&link.target).filter(|&to| to != from) else { continue };
                    match at.get(&to) {
                        Some(&slot) => {
                            links[slot].1.links += 1;
                            links[slot].1.nofollow &= link.nofollow;
                        }
                        None => {
                            at.insert(to, links.len());
                            links.push((to, Source { from_id, links: 1, redirect: false, nofollow: link.nofollow }));
                        }
                    }
                }
                if let Some(to) = page.location.as_deref().and_then(find).filter(|&to| to != from) {
                    links.push((to, Source { from_id, links: 1, redirect: true, nofollow: false }));
                }
                links
            })
            .collect();
        for (to, source) in found.into_iter().flatten() {
            graph[to as usize].1.push(source);
        }
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_only_crawls_keep_head_and_response_checks_only() {
        let head = ExtractOptions::head_only();
        assert!(!meaningful("dead_end", &head));
        assert!(!meaningful("orphan_page", &head));
        assert!(!meaningful("h1_missing", &head));
        assert!(!meaningful("little_content", &head));
        assert!(meaningful("title_missing", &head));
        assert!(meaningful("canonical_relative", &head));
        assert!(meaningful("client_error_4xx", &head));
        let all = ExtractOptions::default();
        for check in seo_checks::CATALOG {
            assert!(meaningful(check.code, &all), "{} needs nothing beyond a full extraction", check.code);
        }
    }
}
