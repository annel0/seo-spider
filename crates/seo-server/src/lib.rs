//! The web UI's server: a JSON API to start, pause and stop a crawl and to
//! read its tables, progress as server-sent events, and the UI itself.
//!
//! One crawl runs at a time, into the server's crawl file. Every read opens
//! that file read-only next to the crawl writing it.

mod assets;
mod compare;
mod content;
mod crawls;
mod links;
mod tabs;

use std::collections::HashMap;
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
use seo_store::{RowQuery, Store};
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
    /// Read connections kept between requests: a database server's cost a
    /// connection (and a thread) each to open.
    readers: std::sync::Mutex<Vec<Store>>,
    /// One link graph is built at a time, once per crawl (see `links`).
    graphs: Mutex<()>,
    graphed: Mutex<std::collections::HashSet<i64>>,
    /// Render pages for the detail panel, made on first use: one for
    /// crawls of public sites, one (that may reach private addresses) for
    /// crawls that start at a private one. A public page's scripts never
    /// run where they could reach the local network.
    view_pools: [tokio::sync::OnceCell<seo_crawl::RenderPool>; 2],
}

/// Read connections kept open at most.
const READERS: usize = 4;

/// Serves until the process ends.
pub async fn serve(options: ServerOptions) -> Result<()> {
    if !options.bind.ip().is_loopback() && options.token.is_none() {
        bail!("слушать не на localhost можно только с --token: без него обходом управляет любой, кто видит порт");
    }
    // Creates the file and its schema when there is none yet.
    drop(Store::open(&options.db)?);
    let state =
        Arc::new(AppState {
            db: options.db.clone(),
            running: Mutex::new(None),
            readers: std::sync::Mutex::new(Vec::new()),
            graphs: Mutex::new(()),
            graphed: Mutex::new(Default::default()),
            view_pools: [tokio::sync::OnceCell::new(), tokio::sync::OnceCell::new()],
        });
    let mut api = Router::new()
        .route("/api/presets", get(presets))
        .route("/api/presets/{name}/config", get(preset_config))
        .route("/api/config/schema", get(config_schema))
        .route("/api/crawls", get(list_crawls).post(start_crawl))
        .route("/api/crawls/{id}/pause", post(pause_crawl))
        .route("/api/crawls/{id}/resume", post(resume_crawl))
        .route("/api/crawls/{id}/stop", post(stop_crawl))
        .route("/api/crawls/{id}/continue", post(continue_crawl))
        .route("/api/crawls/{id}/events", get(events))
        .route("/api/crawls/{id}/overview", get(overview))
        .route("/api/crawls/{id}/issues", get(issues))
        .route("/api/crawls/{id}/tabs", get(crawl_tabs))
        .route("/api/crawls/{id}/tabs/{tab}/rows", get(tab_rows))
        .route("/api/crawls/{id}/tabs/{tab}/export.csv", get(export_csv))
        .route("/api/urls/{id}", get(url_detail))
        .route("/api/urls/{id}/inlinks", get(links::inlinks))
        .route("/api/urls/{id}/outlinks", get(links::outlinks))
        .route("/api/urls/{id}/content", get(content::content))
        .route("/api/crawls/{id}/broken-links", get(links::broken_links))
        .route("/api/crawls/{id}/broken-links.csv", get(links::broken_links_csv))
        .route("/api/crawls/{id}/compare", get(compare::compare))
        .route("/api/crawls/{id}/compare.csv", get(compare::compare_csv))
        .route("/api/crawls/{id}/export.xlsx", get(export_xlsx))
        .with_state(state);
    if let Some(token) = options.token.clone() {
        api = api.layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
            let token = token.clone();
            async move { authorize(&token, request, next).await }
        }));
    }
    let app = api.merge(assets::router(options.ui_dir.clone()));
    let listener = tokio::net::TcpListener::bind(options.bind).await?;
    eprintln!("seo-spider ui: http://{}/  (база {})", options.bind, seo_store::describe(options.db.as_os_str()));
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
/// Runs a read on the store off the async threads, on a kept connection
/// when there is one; a connection whose read failed is not kept.
async fn read<T: Send + 'static>(
    state: &Arc<AppState>,
    work: impl FnOnce(&Store) -> Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let kept = state.readers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).pop();
        let store = match kept {
            Some(store) => store,
            None => Store::open_read(&state.db)?,
        };
        let result = work(&store);
        if result.is_ok() {
            let mut readers = state.readers.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if readers.len() < READERS {
                readers.push(store);
            }
        }
        result
    })
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
        Preset { name: "auto", title: "Авто", description: "Сам решает, нужен ли JavaScript: открывает главную и по странице из каждого раздела в браузере и сравнивает с HTML. Если JS добавляет текст или ссылки или меняет title, H1, canonical, robots — включает рендер на весь обход, иначе обходит без него, быстро." },
        Preset { name: "headers", title: "Только заголовки", description: "Только код ответа и заголовки для стартового адреса и sitemap. Страницы не скачиваются, по ссылкам не ходит." },
        Preset { name: "minimal", title: "Минимум", description: "Читает только <head> страниц из стартового адреса и sitemap: title, description, canonical, robots. По ссылкам не ходит." },
        Preset { name: "seo", title: "SEO", description: "Скачивает весь HTML, ходит по ссылкам, проверяет картинки, скрипты и внешние ссылки. JavaScript не выполняет." },
        Preset { name: "js", title: "SEO + JS", description: "Как SEO, но каждую страницу ещё и открывает движок браузера и выполняет её JavaScript: видно то, что видит Google. В несколько раз медленнее." },
        Preset { name: "dump", title: "Дамп", description: "Как SEO + JS, плюс сохраняет исходный и отрисованный HTML каждой страницы на диск." },
    ])
}

/// A preset's settings in full, as the settings form starts from them.
async fn preset_config(Path(name): Path<String>) -> ApiResult<seo_crawl::CrawlConfig> {
    seo_crawl::CrawlConfig::preset(&name)
        .map(Json)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("нет пресета {name:?}")))
}

/// The JSON Schema of the crawl settings: the settings form is built from it.
async fn config_schema() -> Json<serde_json::Value> {
    Json(serde_json::to_value(schemars::schema_for!(seo_crawl::CrawlConfig)).unwrap_or_default())
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
                    _ => closed_state(&info),
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

/// Where a crawl that is not running was left.
fn closed_state(info: &seo_store::CrawlInfo) -> &'static str {
    match (&info.finished_at, info.stopped) {
        (Some(_), true) => "stopped",
        (Some(_), false) => "finished",
        (None, _) => "interrupted",
    }
}

/// Goes on with an interrupted or stopped crawl (or a finished one, with a
/// higher limit). One crawl runs at a time.
async fn continue_crawl(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(request): Json<crawls::ContinueRequest>,
) -> Result<(StatusCode, Json<Started>), ApiError> {
    let mut running = state.running.lock().await;
    if running.as_ref().is_some_and(Running::is_active) {
        return Err(ApiError(StatusCode::CONFLICT, "обход уже идёт: остановите его или дождитесь конца".into()));
    }
    let continued = crawls::resume(state.db.clone(), id, request)
        .await
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("{error:#}")))?;
    *running = Some(continued);
    Ok((StatusCode::OK, Json(Started { id })))
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
    /// probing (the auto mode reads pages both ways), crawling, paused,
    /// stopping, checking, finished, stopped, failed, interrupted.
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
        Phase::Crawling if progress.probing => ("probing", None),
        Phase::Crawling if progress.paused => ("paused", None),
        Phase::Crawling => ("crawling", None),
        Phase::Checking => ("checking", None),
        Phase::Finished if progress.stopping => ("stopped", None),
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
                state: closed_state(&info),
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
        let last = matches!(event.state, "finished" | "stopped" | "failed" | "interrupted");
        let sse = Event::default().event("progress").json_data(&event).unwrap_or_default();
        // After the last event the stream ends on the next poll.
        let next = if last { (None, None, false) } else { (running, None, false) };
        Some((Ok(sse), next))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Serialize)]
struct IssueCount {
    code: String,
    /// URLs with the issue.
    count: i64,
    title: Option<&'static str>,
    description: Option<&'static str>,
    severity: Option<&'static str>,
    category: Option<&'static str>,
    /// The tab that lists it as a filter.
    tab: Option<&'static str>,
}

fn issue_info(code: String, count: i64) -> IssueCount {
    let check = seo_checks::find(&code);
    IssueCount {
        count,
        title: check.map(|check| check.title),
        description: check.map(|check| check.description),
        severity: check.map(|check| check.severity.as_str()),
        category: check.map(|check| check.category),
        tab: check.and_then(tabs::tab_of_issue),
        code,
    }
}

/// The issues the crawl found, most widespread first.
async fn issues(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult<Vec<IssueCount>> {
    let issues = read(&state, move |store| store.issue_counts(id)).await?;
    Ok(Json(issues.into_iter().map(|(code, count)| issue_info(code, count)).collect()))
}

/// A tab for the UI: its columns, and its filters with how many URLs each
/// keeps.
#[derive(Serialize)]
struct TabReply {
    key: &'static str,
    title: &'static str,
    /// URLs the tab lists.
    total: i64,
    columns: Vec<tabs::Column>,
    filters: Vec<serde_json::Value>,
}

/// The tabs of a crawl, with counts.
async fn crawl_tabs(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult<Vec<TabReply>> {
    let replies = read(&state, move |store| {
        let tabs = crawl_tab_views(store, id)?;
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        for tab in &tabs {
            for filter in std::iter::once(&"all").chain(tab.filters) {
                if !pairs.contains(&(tab.scope, *filter)) {
                    pairs.push((tab.scope, *filter));
                }
            }
        }
        let counts: HashMap<(&str, &str), i64> = pairs.iter().copied().zip(store.filter_counts(id, &pairs)?).collect();
        let issues: HashMap<String, i64> = store.issue_counts(id)?.into_iter().collect();
        Ok(tabs
            .into_iter()
            .map(|tab| TabReply {
                key: tab.key,
                title: tab.title,
                total: counts.get(&(tab.scope, "all")).copied().unwrap_or(0),
                filters: tabs::filters(&tab, &counts, &issues),
                columns: tab.columns,
            })
            .collect())
    })
    .await?;
    Ok(Json(replies))
}

fn crawl_tab_views(store: &Store, id: i64) -> Result<Vec<tabs::TabView>> {
    let config = store.crawl_config(id).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    Ok(tabs::tabs_of(&config))
}

fn crawl_tab(store: &Store, id: i64, key: &str) -> Result<tabs::TabView> {
    match crawl_tab_views(store, id)?.into_iter().find(|tab| tab.key == key) {
        Some(tab) => Ok(tab),
        None => bail!("нет вкладки {key:?}"),
    }
}

#[derive(Deserialize)]
struct RowsParams {
    filter: Option<String>,
    q: Option<String>,
    /// A column key of the tab.
    sort: Option<String>,
    desc: Option<bool>,
    offset: Option<i64>,
    limit: Option<i64>,
}

impl RowsParams {
    /// The store's query for a tab: an issue filter lists every URL with
    /// the issue, whatever the tab's scope.
    fn query(&self, tab: &tabs::TabView) -> RowQuery {
        let filter = self.filter.clone().unwrap_or_else(|| "all".into());
        let sort = self
            .sort
            .as_deref()
            .and_then(|key| tab.columns.iter().find(|column| column.key == key))
            .and_then(|column| column.sort)
            .unwrap_or("url");
        RowQuery {
            scope: if filter.starts_with("issue:") { "all".into() } else { tab.scope.into() },
            filter,
            search: self.q.clone().filter(|text| !text.trim().is_empty()),
            sort: sort.into(),
            descending: self.desc.unwrap_or(false),
            offset: self.offset.unwrap_or(0),
            limit: self.limit.unwrap_or(100),
        }
    }
}

#[derive(Serialize)]
struct TabRows {
    total: i64,
    rows: Vec<TabRow>,
}

#[derive(Serialize)]
struct TabRow {
    id: i64,
    cells: Vec<serde_json::Value>,
}

async fn tab_rows(
    State(state): State<Arc<AppState>>,
    Path((id, tab)): Path<(i64, String)>,
    Query(params): Query<RowsParams>,
) -> ApiResult<TabRows> {
    let rows = read(&state, move |store| {
        let tab = crawl_tab(store, id, &tab)?;
        let page = store.url_rows(id, &params.query(&tab))?;
        Ok(TabRows {
            total: page.total,
            rows: page.rows.iter().map(|row| TabRow { id: row.url.id, cells: tabs::cells(&tab.columns, row) }).collect(),
        })
    })
    .await?;
    Ok(Json(rows))
}

/// A tab as CSV, every row its filter, search and sort keep; with a BOM so
/// that Excel reads it as UTF-8.
async fn export_csv(
    State(state): State<Arc<AppState>>,
    Path((id, tab)): Path<(i64, String)>,
    Query(params): Query<RowsParams>,
) -> Result<Response, ApiError> {
    let (bytes, name) = read(&state, move |store| {
        let tab = crawl_tab(store, id, &tab)?;
        let mut writer = csv::Writer::from_writer(b"\xEF\xBB\xBF".to_vec());
        writer.write_record(tab.columns.iter().map(|column| column.title.as_str()))?;
        let mut query = params.query(&tab);
        query.offset = 0;
        query.limit = 2000;
        loop {
            let page = store.url_rows(id, &query)?;
            for row in &page.rows {
                writer.write_record(tabs::cells(&tab.columns, row).iter().map(tabs::csv_text))?;
            }
            query.offset += page.rows.len() as i64;
            if page.rows.is_empty() || query.offset >= page.total {
                break;
            }
        }
        let name = match query.filter.as_str() {
            "all" => tab.key.to_string(),
            filter => format!("{}-{}", tab.key, filter.trim_start_matches("issue:")),
        };
        Ok((writer.into_inner().map_err(|error| anyhow::anyhow!("{error}"))?, name))
    })
    .await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}-{id}.csv\"")),
        ],
        bytes,
    )
        .into_response())
}

/// The overview's charts: counts by group, and issues by severity.
#[derive(Serialize)]
struct OverviewReply {
    #[serde(flatten)]
    counts: seo_store::Overview,
    /// URLs with an issue of each severity, and issues found in all.
    severities: Vec<(&'static str, i64)>,
    /// What the auto mode decided about JavaScript, for a crawl run in it.
    render_decision: Option<seo_crawl::RenderDecision>,
}

async fn overview(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> ApiResult<OverviewReply> {
    let (counts, issues, config) =
        read(&state, move |store| Ok((store.overview(id)?, store.issue_counts(id)?, store.crawl_config(id)?))).await?;
    let render_decision = serde_json::from_str::<serde_json::Value>(&config)
        .ok()
        .and_then(|config| serde_json::from_value(config["render_decision"].clone()).ok());
    let mut severities = vec![("high", 0), ("medium", 0), ("low", 0)];
    for (code, count) in issues {
        if let Some(check) = seo_checks::find(&code) {
            if let Some(entry) = severities.iter_mut().find(|(name, _)| *name == check.severity.as_str()) {
                entry.1 += count;
            }
        }
    }
    Ok(Json(OverviewReply { counts, severities, render_decision }))
}

/// Every tab of the crawl as one XLSX workbook, as `seo-spider export`
/// writes it.
async fn export_xlsx(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Result<Response, ApiError> {
    let db = state.db.clone();
    let (bytes, start_url) = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, String)> {
        let store = Store::open_read(&db)?;
        let start_url = store.crawls()?.into_iter().find(|crawl| crawl.id == id).map(|crawl| crawl.start_url).unwrap_or_default();
        let path = std::env::temp_dir().join(format!("seo-spider-export-{}-{id}.xlsx", std::process::id()));
        let mut sink: Box<dyn seo_export::Sink> = Box::new(seo_export::XlsxSink::new(&path));
        seo_export::export(&store, id, sink.as_mut())?;
        sink.finish()?;
        let bytes = std::fs::read(&path)?;
        let _ = std::fs::remove_file(&path);
        Ok((bytes, start_url))
    })
    .await
    .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))??;
    let host = url::Url::parse(&start_url).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_else(|| "crawl".into());
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{host}-{id}.xlsx\"")),
        ],
        bytes,
    )
        .into_response())
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
