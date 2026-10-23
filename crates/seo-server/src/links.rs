//! Links of one URL and of the crawl: the pages that link to a URL, the
//! links a page has with their targets' statuses, and every broken link
//! with where it sits. The graph (which page links to which) is the
//! crawl's `inlinks`; anchors come from the source pages' facts, read only
//! for the rows asked for.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use seo_crawl::LinkTargets;
use seo_store::{LinkRow, Store, REDIRECT};
use serde::{Deserialize, Serialize};

use crate::{read, ApiError, ApiResult, AppState};

#[derive(Deserialize)]
pub(crate) struct Page {
    offset: Option<i64>,
    limit: Option<i64>,
}

/// A link between two of the crawl's URLs, as the views list it.
#[derive(Serialize)]
pub(crate) struct LinkItem {
    from_id: i64,
    from_url: String,
    to_id: i64,
    to_url: String,
    /// The target's status.
    status: u16,
    /// A redirect (or meta refresh) rather than links.
    redirect: bool,
    /// Links on the page to the target.
    links: i64,
    nofollow: bool,
    /// Anchor texts, each once.
    anchors: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct LinkPage {
    /// The crawl's link graph exists (it is written when the crawl's
    /// checks end).
    ready: bool,
    total: i64,
    rows: Vec<LinkItem>,
}

/// A link as the page has it, with its target's row when the crawl
/// requested it.
#[derive(Serialize)]
pub(crate) struct Outlink {
    url: String,
    anchor: String,
    nofollow: bool,
    /// header, nav, footer, aside or content.
    position: &'static str,
    image: bool,
    id: Option<i64>,
    status: Option<u16>,
}

fn targets(store: &Store, crawl_id: i64) -> Result<LinkTargets> {
    LinkTargets::new(&serde_json::from_str(&store.crawl_config(crawl_id)?).unwrap_or_default())
}

/// The anchors of each source's links to its target, from the sources'
/// facts (each page read once).
fn with_anchors(store: &Store, crawl_id: i64, rows: Vec<LinkRow>) -> Result<Vec<LinkItem>> {
    let targets = targets(store, crawl_id)?;
    let mut pages: HashMap<i64, Vec<(Option<String>, String, bool)>> = HashMap::new();
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let links = match pages.entry(row.from_id) {
            Entry::Occupied(found) => found.into_mut(),
            Entry::Vacant(slot) => slot.insert(
                store
                    .url(row.from_id)?
                    .and_then(|page| {
                        page.facts().map(|facts| {
                            facts.links.iter().map(|link| (targets.target(&link.href), link.anchor.clone(), link.image)).collect()
                        })
                    })
                    .unwrap_or_default(),
            ),
        };
        let mut anchors: Vec<String> = Vec::new();
        if row.kind != REDIRECT {
            let mut blank = false;
            for (target, anchor, image) in links.iter() {
                if target.as_deref() != Some(row.to_url.as_str()) {
                    continue;
                }
                let text = match (anchor.trim(), image) {
                    ("", true) => "[картинка]",
                    ("", false) => {
                        blank = true;
                        continue;
                    }
                    (text, _) => text,
                };
                if !anchors.iter().any(|have| have == text) {
                    anchors.push(text.to_string());
                }
            }
            // A link with no text at all says so; beside others it is noise.
            if anchors.is_empty() && blank {
                anchors.push("[без текста]".into());
            }
        }
        items.push(LinkItem {
            from_id: row.from_id,
            from_url: row.from_url,
            to_id: row.to_id,
            to_url: row.to_url,
            status: row.status,
            redirect: row.kind == REDIRECT,
            links: row.links,
            nofollow: row.nofollow,
            anchors,
        });
    }
    Ok(items)
}

/// The crawl's link graph, written now for a crawl checked before it
/// existed: by running its checks again (same issues, and the graph). Once
/// per crawl and server, and never for the crawl that is running.
async fn ensure_graph(state: &Arc<AppState>, crawl_id: i64) -> Result<bool, ApiError> {
    if read(state, move |store| store.has_edges(crawl_id)).await? {
        return Ok(true);
    }
    let running = state.running.lock().await.as_ref().is_some_and(|running| running.id == crawl_id && running.is_active());
    if running {
        return Ok(false);
    }
    let _one = state.graphs.lock().await;
    if !state.graphed.lock().await.insert(crawl_id) {
        return read(state, move |store| store.has_edges(crawl_id)).await;
    }
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut store = Store::open(&db)?;
        seo_crawl::run_checks(&mut store, crawl_id).context("проверки и граф ссылок")?;
        Ok(())
    })
    .await
    .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))??;
    read(state, move |store| store.has_edges(crawl_id)).await
}

/// The pages that link or redirect to a URL, with their anchors.
pub(crate) async fn inlinks(State(state): State<Arc<AppState>>, Path(id): Path<i64>, Query(page): Query<Page>) -> ApiResult<LinkPage> {
    let crawl_id = read(&state, move |store| store.url_crawl(id))
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "нет такого URL".into()))?;
    if !ensure_graph(&state, crawl_id).await? {
        return Ok(Json(LinkPage { ready: false, total: 0, rows: Vec::new() }));
    }
    let (offset, limit) = (page.offset.unwrap_or(0), page.limit.unwrap_or(100));
    let reply = read(&state, move |store| {
        let (total, rows) = store.inlinks(crawl_id, id, offset, limit)?;
        Ok(LinkPage { ready: true, total, rows: with_anchors(store, crawl_id, rows)? })
    })
    .await?;
    Ok(Json(reply))
}

/// A page's links as it has them, each with its target's status when the
/// crawl requested it.
pub(crate) async fn outlinks(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult<Vec<Outlink>> {
    let reply = read(&state, move |store| {
        let Some(crawl_id) = store.url_crawl(id)? else { return Ok(Vec::new()) };
        let Some(page) = store.url(id)? else { return Ok(Vec::new()) };
        let Some(facts) = page.facts() else { return Ok(Vec::new()) };
        let targets = targets(store, crawl_id)?;
        let resolved: Vec<Option<String>> = facts.links.iter().map(|link| targets.target(&link.href)).collect();
        let addresses: Vec<&str> = resolved.iter().flatten().map(String::as_str).collect();
        let found = store.urls_by_address(crawl_id, &addresses)?;
        Ok(facts
            .links
            .iter()
            .zip(&resolved)
            .map(|(link, target)| {
                let row = target.as_ref().and_then(|target| found.get(target));
                Outlink {
                    url: target.clone().unwrap_or_else(|| link.href_raw.clone()),
                    anchor: link.anchor.clone(),
                    nofollow: link.nofollow,
                    position: link.position.as_str(),
                    image: link.image,
                    id: row.map(|(id, _)| *id),
                    status: row.map(|(_, status)| *status),
                }
            })
            .collect())
    })
    .await?;
    Ok(Json(reply))
}

/// Every link of the crawl whose target is broken, with where it sits.
pub(crate) async fn broken_links(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult<LinkPage> {
    if !ensure_graph(&state, id).await? {
        return Ok(Json(LinkPage { ready: false, total: 0, rows: Vec::new() }));
    }
    let (offset, limit) = (page.offset.unwrap_or(0), page.limit.unwrap_or(200));
    let reply = read(&state, move |store| {
        let (total, rows) = store.broken_links(id, offset, limit)?;
        Ok(LinkPage { ready: true, total, rows: with_anchors(store, id, rows)? })
    })
    .await?;
    Ok(Json(reply))
}

/// The broken links as CSV, for whoever fixes the pages.
pub(crate) async fn broken_links_csv(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Result<Response, ApiError> {
    ensure_graph(&state, id).await?;
    let bytes = read(&state, move |store| {
        let mut writer = csv::Writer::from_writer(b"\xEF\xBB\xBF".to_vec());
        writer.write_record(["Страница", "Ссылка на", "Код", "Анкор", "Ссылок", "nofollow", "Редирект"])?;
        let mut offset = 0;
        loop {
            let (total, rows) = store.broken_links(id, offset, 2000)?;
            offset += rows.len() as i64;
            for item in with_anchors(store, id, rows)? {
                writer.write_record([
                    item.from_url,
                    item.to_url,
                    item.status.to_string(),
                    item.anchors.join(" | "),
                    item.links.to_string(),
                    if item.nofollow { "да" } else { "нет" }.into(),
                    if item.redirect { "да" } else { "нет" }.into(),
                ])?;
            }
            if offset >= total || total == 0 {
                break;
            }
        }
        writer.into_inner().map_err(|error| anyhow::anyhow!("{error}"))
    })
    .await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"broken-links-{id}.csv\"")),
        ],
        bytes,
    )
        .into_response())
}
