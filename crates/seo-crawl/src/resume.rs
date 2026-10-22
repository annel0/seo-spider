//! Going on with a crawl that was interrupted or stopped. Its queue was
//! never written: it is found again from what was. The URLs the crawl wrote
//! are known first, with their ids and their share of the budgets; then the
//! links of its pages are discovered again from their stored facts by the
//! same rules, which gives the click edges the depths need and queues what
//! no row has yet. The order of the queue may differ from the first run's;
//! what gets crawled does not.

use rayon::prelude::*;
use seo_store::{SavedUrl, Store};

use crate::{discover_from, CrawlSummary, Frontier, Rules};

/// What `register` leaves for `replay`: the click depth and resource flag
/// of each URL written, by its index (they are known first, so 0..n).
pub(crate) struct Registered {
    seeds: Vec<(u32, bool)>,
    /// Internal URLs written, for robots.txt to judge again.
    pub internal: Vec<usize>,
}

const BLOCKED: &str = "blocked by robots.txt";

/// Knows every URL the crawl wrote, in the order it found them. A URL whose
/// depth the crawl never settled (an interrupted crawl writes the depth it
/// found it at; a sitemap-only page has none) counts as one click deep, as
/// the sitemap's URLs are queued.
pub(crate) fn register(store: &Store, crawl_id: i64, frontier: &mut Frontier) -> anyhow::Result<Registered> {
    let saved = store.saved_urls(crawl_id)?;
    let mut seeds = Vec::with_capacity(saved.len());
    let mut internal = Vec::new();
    for url in saved {
        let resource = is_resource(&url);
        let blocked = url.blocked_by_robots || url.error.as_deref() == Some(BLOCKED);
        let index = frontier.register(url.url, url.id, url.internal, resource, blocked);
        seeds.push((url.depth.unwrap_or(1), resource));
        if url.internal {
            internal.push(index);
        }
    }
    Ok(Registered { seeds, internal })
}

/// Which budget an internal URL drew on is not stored: what came back as
/// something other than HTML, and not as a redirect or an error, was an
/// image, script, style or other file.
fn is_resource(url: &SavedUrl) -> bool {
    let html = url.content_type.as_deref().is_some_and(|kind| {
        let kind = kind.to_ascii_lowercase();
        kind.starts_with("text/html") || kind.starts_with("application/xhtml")
    });
    url.internal && url.content_type.is_some() && !html && (200..300).contains(&url.status)
}

/// Discovers again from the stored facts of every page written, a chunk at
/// a time (decoded and read in parallel). Queues what it finds that no row
/// has, and pages whose response was written but whose facts were not (the
/// run ended between the two): they are requested again under their id,
/// which it returns for their rows to be removed first. Counts what was
/// written into `summary`.
pub(crate) fn replay(
    store: &Store,
    crawl_id: i64,
    frontier: &mut Frontier,
    registered: &Registered,
    rules: &Rules,
    reads_documents: bool,
    summary: &mut CrawlSummary,
) -> anyhow::Result<Vec<i64>> {
    let mut again = Vec::new();
    store.for_each_url_chunk(crawl_id, 1024, |chunk| {
        let found: Vec<Found> = {
            let frontier = &*frontier;
            chunk
                .into_par_iter()
                .filter_map(|encoded| {
                    let index = *frontier.index.get(&encoded.url.http.url)?;
                    let (depth, resource) = *registered.seeds.get(index)?;
                    let had_render = encoded.render.as_ref().map(|(error, ..)| error.is_some());
                    let unread = reads_documents
                        && encoded.raw.is_none()
                        && encoded.url.internal
                        && encoded.url.http.is_html()
                        && (200..300).contains(&encoded.url.http.status);
                    if unread {
                        return Some(Found { index, depth, again: true, candidates: Vec::new(), page: false, had_render: None, error: false });
                    }
                    let stored = encoded.decode().ok()?;
                    let http = &stored.http;
                    let error = (http.status == 0 && http.error.as_deref() != Some(BLOCKED)) || http.status >= 400;
                    let documents = [stored.raw.as_ref(), stored.rendered.as_ref()];
                    let candidates = discover_from(depth, resource, http, documents, rules);
                    Some(Found { index, depth, again: false, candidates, page: stored.raw.is_some(), had_render, error })
                })
                .collect()
        };
        for found in found {
            if found.again {
                frontier.requeue(found.index, found.depth);
                again.push(frontier.known[found.index].id);
                continue;
            }
            summary.urls += 1;
            summary.pages += usize::from(found.page);
            summary.errors += usize::from(found.error);
            if let Some(failed) = found.had_render {
                summary.rendered += 1;
                summary.render_errors += usize::from(failed);
            }
            for candidate in found.candidates {
                frontier.push(candidate.url, candidate.depth, candidate.internal, candidate.resource, Some(found.index), candidate.edge);
            }
        }
        Ok(())
    })?;
    Ok(again)
}

struct Found {
    index: usize,
    depth: u32,
    /// Written without its facts: request it again.
    again: bool,
    candidates: Vec<crate::Candidate>,
    page: bool,
    /// Rendered, and whether the render failed.
    had_render: Option<bool>,
    error: bool,
}
