//! A page's content for the detail panel: its documents (as the server
//! sent it, and rendered) with their visible text in blocks, which the UI
//! compares to show what JavaScript added; blocks of the site's template
//! (the crawl met them on many pages: menus, header, footer) are marked.
//! The crawl's own documents when it kept them (`store.raw_html`,
//! `store.rendered_html`: the dump preset); otherwise, when asked, the
//! page fetched and rendered now.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use seo_page::{block_hash, text_blocks};
use seo_store::{BodyKind, Store};
use serde::{Deserialize, Serialize};

use crate::{read, ApiError, ApiResult, AppState};

/// HTML longer than this is cut for the browser; the text is whole.
const HTML_SHOWN: usize = 2 << 20;

#[derive(Serialize)]
pub(crate) struct Block {
    tag: String,
    text: String,
    /// Pages of the crawl the block is on, when it is the site's template.
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<u32>,
}

#[derive(Serialize)]
pub(crate) struct Document {
    html: String,
    /// The HTML was cut at `HTML_SHOWN`.
    cut: bool,
    blocks: Vec<Block>,
}

#[derive(Serialize)]
pub(crate) struct Content {
    /// The documents are the crawl's own.
    kept: bool,
    /// The documents were fetched and rendered now.
    live: bool,
    raw: Option<Document>,
    rendered: Option<Document>,
    /// Why one of them is missing.
    error: Option<String>,
    /// The crawl knows its site's template (crawls before it was kept
    /// do not).
    templated: bool,
}

#[derive(Deserialize)]
pub(crate) struct Ask {
    /// Fetch and render the page now.
    live: Option<bool>,
}

type Template = HashMap<u64, u32>;

fn document(html: String, template: &Template) -> Document {
    let blocks = text_blocks(&html)
        .into_iter()
        .map(|block| Block { template: template.get(&block_hash(&block.tag, &block.text)).copied(), tag: block.tag, text: block.text })
        .collect();
    let cut = html.len() > HTML_SHOWN;
    let html = if cut { html[..html.floor_char_boundary(HTML_SHOWN)].to_string() } else { html };
    Document { html, cut, blocks }
}

fn kept(store: &Store, url_id: i64, kind: BodyKind) -> Result<Option<String>> {
    let Some((encoding, data)) = store.body(url_id, kind)? else { return Ok(None) };
    let bytes = match encoding.as_str() {
        "zstd" => zstd::stream::decode_all(data.as_slice())?,
        _ => data,
    };
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The page's documents: the crawl's, or (`live=true`) fetched and rendered
/// now with the crawl's settings.
pub(crate) async fn content(State(state): State<Arc<AppState>>, Path(id): Path<i64>, Query(ask): Query<Ask>) -> ApiResult<Content> {
    if !ask.live.unwrap_or(false) {
        let reply = read(&state, move |store| {
            let (raw, rendered) = (kept(store, id, BodyKind::Raw)?, kept(store, id, BodyKind::Rendered)?);
            let kept = raw.is_some() || rendered.is_some();
            let template = match store.url_crawl(id)? {
                Some(crawl_id) if kept => store.template_map(crawl_id)?,
                _ => Template::new(),
            };
            Ok(Content {
                kept,
                live: false,
                raw: raw.map(|html| document(html, &template)),
                rendered: rendered.map(|html| document(html, &template)),
                error: None,
                templated: !template.is_empty(),
            })
        })
        .await?;
        return Ok(Json(reply));
    }
    let (address, config, template) = read(&state, move |store| {
        let page = store.url(id)?.ok_or_else(|| anyhow!("нет такого URL"))?;
        let crawl_id = store.url_crawl(id)?.ok_or_else(|| anyhow!("нет такого URL"))?;
        let config: seo_crawl::CrawlConfig = serde_json::from_str(&store.crawl_config(crawl_id)?).unwrap_or_default();
        Ok((page.http.url, config, store.template_map(crawl_id)?))
    })
    .await?;
    let url = url::Url::parse(&address).map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))?;
    let mut errors = Vec::new();
    let mut downloaded = None;
    let raw = match seo_crawl::fetch_now(&config, &url).await {
        Ok((_, Some((html, document)))) => {
            downloaded = Some(document);
            Some(html)
        }
        Ok((http, None)) => {
            errors.push(format!("HTML: ответ {}{}", http.status, http.error.map(|error| format!(", {error}")).unwrap_or_default()));
            None
        }
        Err(error) => {
            errors.push(format!("HTML: {error:#}"));
            None
        }
    };
    let private = config.start_urls.iter().any(seo_crawl::is_private_host);
    let pool = state.view_pools[usize::from(private)]
        .get_or_try_init(|| async { seo_crawl::view_render_pool(&config) })
        .await
        .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("рендер: {error:#}")))?;
    // The document just read, not a second request: what differs is what
    // JavaScript did.
    let rendered = pool.render_document(address, downloaded).await;
    let rendered = match rendered.error {
        Some(error) => {
            errors.push(format!("рендер: {error}"));
            None
        }
        None => Some(rendered.html),
    };
    let reply = tokio::task::spawn_blocking(move || Content {
        kept: false,
        live: true,
        raw: raw.map(|html| document(html, &template)),
        rendered: rendered.map(|html| document(html, &template)),
        error: (!errors.is_empty()).then(|| errors.join("; ")),
        templated: !template.is_empty(),
    })
    .await
    .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(reply))
}
