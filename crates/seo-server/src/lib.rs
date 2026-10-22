//! The web UI's server: a JSON API to start, pause and stop a crawl and to
//! read its tables, progress as server-sent events, and the UI itself.
//!
//! One crawl runs at a time, into the server's crawl file. Every read opens
//! that file read-only next to the crawl writing it.

mod assets;
mod crawls;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use seo_store::{RowQuery, RowsPage, Store};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

pub use crawls::StartRequest;
use crawls::{Phase, Running};

/// How the server runs.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// The crawl file: crawls started here go into it, and its earlier
    /// crawls can be opened.
    pub db: PathBuf,
    pub bind: SocketAddr,
    /// The built UI (`ui/dist`); without it the UI built into the binary,
    /// if any.
    pub ui_dir: Option<PathBuf>,
    /// Required on every API request when set (`Authorization: Bearer`).
    pub token: Option<String>,
}

struct AppState {
    db: PathBuf,
    running: Mutex<Option<Running>>,
}

/// Serves until the process ends.
pub async fn serve(options: ServerOptions) -> Result<()> {
    if !options.bind.ip().is_loopback() && options.token.is_none() {
        bail!("слушать не на localhost можно только с --token: без него обходом управляет любой, кто видит порт");
    }
    // Creates the file and its schema when there is none yet.
    drop(Store::open(&options.db)?);
    let state = Arc::new(AppState { db: options.db.clone(), running: Mutex::new(None) });
    let mut api = Router::new()
        .route("/api/presets", get(presets))
        .route("/api/crawls", get(list_crawls).post(start_crawl))
        .route("/api/crawls/{id}/pause", post(pause_crawl))
        .route("/api/crawls/{id}/resume", post(resume_crawl))
        .route("/api/crawls/{id}/stop", post(stop_crawl))
        .route("/api/crawls/{id}/events", get(events))
        .route("/api/crawls/{id}/summary", get(summary))
        .route("/api/crawls/{id}/rows", get(rows))
        .route("/api/urls/{id}", get(url_detail))
        .with_state(state);
    if let Some(token) = options.token.clone() {
        api = api.layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
            let token = token.clone();
            async move { authorize(&token, request, next).await }
        }));
    }
    let app = api.merge(assets::router(options.ui_dir.clone()));
    let listener = tokio::net::TcpListener::bind(options.bind).await?;
    eprintln!("seo-spider ui: http://{}/  (база {})", options.bind, options.db.display());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn authorize(token: &str, request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let header = request.headers().get("authorization").and_then(|value| value.to_str().ok());
    let query = request.uri().query().unwrap_or("");
    let given = header
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| query.split('&').find_map(|pair| pair.strip_prefix("token=")));
    if given == Some(token) {
        next.run(request).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "нужен токен".into()).into_response()
    }
}

/// An API error: status and a message for the user, in Russian.
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Runs a read on the crawl file off the async threads.
async fn read<T: Send + 'static>(state: &AppState, work: impl FnOnce(&Store) -> Result<T> + Send + 'static) -> Result<T, ApiError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || work(&Store::open_read(&db)?))
        .await
        .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("{error:#}")))
}

#[derive(Serialize)]
struct Preset {
    name: &'static str,
    title: &'static str,
    description: &'static str,
}

async fn presets() -> Json<Vec<Preset>> {
    Json(vec![
        Preset { name: "headers", title: "Только заголовки", description: "Статус и заголовки ответа (HEAD) для стартовых URL и sitemap; тело не скачивается." },
        Preset { name: "minimal", title: "Минимум", description: "Заголовки и <head> страниц из стартовых URL и sitemap: title, description, canonical, robots." },
        Preset { name: "seo", title: "SEO-аудит", description: "Весь HTML, обход по ссылкам, проверка ресурсов и внешних ссылок." },
        Preset { name: "js", title: "С рендером", description: "Как SEO-аудит, плюс рендер JavaScript: страница такой, какой её видит браузер." },
        Preset { name: "dump", title: "Полный дамп", description: "С рендером и с сохранением сырого и отрисованного HTML каждой страницы." },
    ])
}

/// A crawl in the list, with where it is.
#[derive(Serialize)]
struct CrawlItem {
    #[serde(flatten)]
    info: seo_store::CrawlInfo,
    state: &'static str,
}

async fn list_crawls(State(state): State<Arc<AppState>>) -> ApiResult<Vec<CrawlItem>> {
    let running = state.running.lock().await.clone();
    let crawls = read(&state, |store| store.crawls()).await?;
    Ok(Json(
        crawls
            .into_iter()
            .map(|info| {
                let state = match &running {
                    Some(running) if running.id == info.id => progress_event(running).state,
                    _ if info.finished_at.is_some() => "finished",
                    _ => "interrupted",
                };
                CrawlItem { info, state }
            })
            .collect(),
    ))
}

#[derive(Serialize)]
struct Started {
    id: i64,
}

async fn start_crawl(State(state): State<Arc<AppState>>, Json(request): Json<StartRequest>) -> Result<(StatusCode, Json<Started>), ApiError> {
    let mut running = state.running.lock().await;
    if running.as_ref().is_some_and(Running::is_active) {
        return Err(ApiError(StatusCode::CONFLICT, "обход уже идёт: остановите его или дождитесь конца".into()));
    }
    let config = crawls::build_config(&request).map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("{error:#}")))?;
    let started = crawls::start(state.db.clone(), config)
        .await
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("{error:#}")))?;
    let id = started.id;
    *running = Some(started);
    Ok((StatusCode::CREATED, Json(Started { id })))
}

/// The running crawl when it is the one asked for.
async fn running_crawl(state: &AppState, id: i64) -> Result<Running, ApiError> {
    match &*state.running.lock().await {
        Some(running) if running.id == id && running.is_active() => Ok(running.clone()),
        _ => Err(ApiError(StatusCode::CONFLICT, "этот обход не идёт".into())),
    }
}

async fn pause_crawl(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    running_crawl(&state, id).await?.pause(true);
    Ok(StatusCode::NO_CONTENT)
}

async fn resume_crawl(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    running_crawl(&state, id).await?.pause(false);
    Ok(StatusCode::NO_CONTENT)
}

async fn stop_crawl(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    running_crawl(&state, id).await?.stop();
    Ok(StatusCode::NO_CONTENT)
}

/// Where a crawl is, as the progress events carry it.
#[derive(Serialize, Clone, PartialEq)]
struct ProgressEvent {
    crawl_id: i64,
    /// crawling, paused, stopping, checking, finished, failed, interrupted.
    state: &'static str,
    error: Option<String>,
    urls: usize,
    pages: usize,
    rendered: usize,
    errors: usize,
    queued: usize,
    in_flight: usize,
    bytes: u64,
    elapsed_ms: u64,
}

fn progress_event(running: &Running) -> ProgressEvent {
    let progress = running.progress.borrow().clone();
    let phase = running.phase.borrow().clone();
    let (state, error) = match phase {
        Phase::Crawling if progress.stopping => ("stopping", None),
        Phase::Crawling if progress.paused => ("paused", None),
        Phase::Crawling => ("crawling", None),
        Phase::Checking => ("checking", None),
        Phase::Finished => ("finished", None),
        Phase::Failed(error) => ("failed", Some(error)),
    };
    ProgressEvent {
        crawl_id: running.id,
        state,
        error,
        urls: progress.urls,
        pages: progress.pages,
        rendered: progress.rendered,
        errors: progress.errors,
        queued: progress.queued,
        in_flight: progress.in_flight,
        bytes: progress.bytes,
        elapsed_ms: progress.elapsed.as_millis() as u64,
    }
}

/// Progress of a crawl, four times a second while it runs; one event for a
/// crawl that is not running.
async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let running = state.running.lock().await.clone().filter(|running| running.id == id);
    let fixed = match &running {
        Some(_) => None,
        None => {
            let crawls = read(&state, |store| store.crawls()).await?;
            let info = crawls
                .into_iter()
                .find(|crawl| crawl.id == id)
                .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "нет такого обхода".into()))?;
            Some(ProgressEvent {
                crawl_id: id,
                state: if info.finished_at.is_some() { "finished" } else { "interrupted" },
                error: None,
                urls: info.urls as usize,
                pages: 0,
                rendered: 0,
                errors: 0,
                queued: 0,
                in_flight: 0,
                bytes: 0,
                elapsed_ms: 0,
            })
        }
    };
    let stream = futures_util::stream::unfold((running, fixed, true), |(running, fixed, first)| async move {
        let event = match (&running, fixed) {
            (Some(running), _) => {
                if !first {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                progress_event(running)
            }
            (None, Some(event)) if first => event,
            _ => return None,
        };
        let last = matches!(event.state, "finished" | "failed" | "interrupted");
        let sse = Event::default().event("progress").json_data(&event).unwrap_or_default();
        // After the last event the stream ends on the next poll.
        let next = if last { (None, None, false) } else { (running, None, false) };
        Some((Ok(sse), next))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Serialize)]
struct Summary {
    filters: Vec<FilterCount>,
    issues: Vec<IssueCount>,
}

#[derive(Serialize)]
struct FilterCount {
    key: &'static str,
    count: i64,
}

#[derive(Serialize)]
struct IssueCount {
    code: String,
    count: i64,
    title: Option<&'static str>,
    description: Option<&'static str>,
    severity: Option<&'static str>,
    category: Option<&'static str>,
}

fn issue_info(code: String, count: i64) -> IssueCount {
    let check = seo_checks::find(&code);
    IssueCount {
        count,
        title: check.map(|check| check.title),
        description: check.map(|check| check.description),
        severity: check.map(|check| check.severity.as_str()),
        category: check.map(|check| check.category),
        code,
    }
}

/// Counts for the filters of the internal table, and the crawl's issues.
async fn summary(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult<Summary> {
    let (filters, issues) = read(&state, move |store| Ok((store.filter_counts(id)?, store.issue_counts(id)?))).await?;
    Ok(Json(Summary {
        filters: filters.into_iter().map(|(key, count)| FilterCount { key, count }).collect(),
        issues: issues.into_iter().map(|(code, count)| issue_info(code, count)).collect(),
    }))
}

#[derive(Deserialize)]
struct RowsParams {
    filter: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    desc: Option<bool>,
    offset: Option<i64>,
    limit: Option<i64>,
}

async fn rows(State(state): State<Arc<AppState>>, Path(id): Path<i64>, Query(params): Query<RowsParams>) -> ApiResult<RowsPage> {
    let query = RowQuery {
        filter: params.filter.unwrap_or_else(|| "all".into()),
        search: params.q,
        sort: params.sort.unwrap_or_else(|| "url".into()),
        descending: params.desc.unwrap_or(false),
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(200),
    };
    Ok(Json(read(&state, move |store| store.url_rows(id, &query)).await?))
}

/// One URL for the detail panel: the response, the facts of the document
/// it is judged by, the render, and its issues.
#[derive(Serialize)]
struct UrlDetail {
    id: i64,
    url: String,
    status: u16,
    content_type: Option<String>,
    location: Option<String>,
    error: Option<String>,
    ttfb_ms: u32,
    size: u64,
    headers: Vec<(String, String)>,
    internal: bool,
    depth: Option<u32>,
    in_sitemap: bool,
    blocked_by_robots: bool,
    facts: Option<FactsSummary>,
    render: Option<seo_page::RenderSummary>,
    issues: Vec<UrlIssue>,
}

#[derive(Serialize)]
struct FactsSummary {
    source: &'static str,
    lang: Option<String>,
    titles: Vec<String>,
    descriptions: Vec<String>,
    headings: Vec<(u8, String)>,
    canonicals: Vec<String>,
    meta_robots: Vec<String>,
    hreflangs: usize,
    word_count: usize,
    links: usize,
    internal_links: usize,
    images: usize,
    images_without_alt: usize,
    json_ld: usize,
    microdata: usize,
    custom: seo_page::CustomFacts,
}

#[derive(Serialize)]
struct UrlIssue {
    code: String,
    detail: Option<String>,
    title: Option<&'static str>,
    description: Option<&'static str>,
    severity: Option<&'static str>,
    category: Option<&'static str>,
}

async fn url_detail(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult<UrlDetail> {
    let (stored, issues) = read(&state, move |store| Ok((store.url(id)?, store.url_issues(id)?))).await?;
    let stored = stored.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "нет такого URL".into()))?;
    let facts = stored.facts().map(|facts| {
        let host = url::Url::parse(&stored.http.url).ok().and_then(|url| url.host_str().map(str::to_string));
        FactsSummary {
            source: facts.source.as_str(),
            lang: facts.lang.clone(),
            titles: facts.titles.clone(),
            descriptions: facts.descriptions.clone(),
            headings: facts.headings.iter().take(50).cloned().collect(),
            canonicals: facts.canonicals.clone(),
            meta_robots: facts.meta_robots.clone(),
            hreflangs: facts.hreflangs.len(),
            word_count: facts.word_count,
            links: facts.links.len(),
            internal_links: facts
                .links
                .iter()
                .filter(|link| url::Url::parse(&link.href).ok().and_then(|url| url.host_str().map(str::to_string)) == host)
                .count(),
            images: facts.images.len(),
            images_without_alt: facts.images.iter().filter(|image| image.alt.is_none()).count(),
            json_ld: facts.json_ld.len(),
            microdata: facts.microdata.len(),
            custom: facts.custom.clone(),
        }
    });
    Ok(Json(UrlDetail {
        id: stored.id,
        url: stored.http.url.clone(),
        status: stored.http.status,
        content_type: stored.http.content_type.clone(),
        location: stored.http.location.clone(),
        error: stored.http.error.clone(),
        ttfb_ms: stored.http.ttfb_ms,
        size: stored.http.size,
        headers: stored.http.headers.clone(),
        internal: stored.internal,
        depth: stored.depth,
        in_sitemap: stored.in_sitemap,
        blocked_by_robots: stored.blocked_by_robots,
        render: stored.render.clone(),
        facts,
        issues: issues
            .into_iter()
            .map(|(code, detail)| {
                let info = issue_info(code, 0);
                UrlIssue {
                    code: info.code,
                    detail,
                    title: info.title,
                    description: info.description,
                    severity: info.severity,
                    category: info.category,
                }
            })
            .collect(),
    }))
}
