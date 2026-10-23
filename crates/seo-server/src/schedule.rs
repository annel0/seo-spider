//! Crawls on a schedule. The server looks every half minute for the ones
//! due and starts them as the UI would (one crawl runs at a time: a due
//! one waits for the running one), and when a crawl ends sends its report
//! to the schedule's Telegram chat.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use seo_store::{Schedule, Store};
use serde::{Deserialize, Serialize};

use crate::crawls::{self, Phase, Running, StartRequest};
use crate::{notify, read, ApiError, ApiResult, AppState};

/// How often due schedules are looked for.
const TICK: Duration = Duration::from_secs(30);

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// The first run after `now`, on the schedule's beat: runs missed while
/// the server was down are not made up.
fn next_after(next_run: i64, every_hours: u32, now: i64) -> i64 {
    let step = i64::from(every_hours.max(1)) * 3600;
    if next_run > now {
        return next_run;
    }
    next_run + ((now - next_run) / step + 1) * step
}

/// Writes go through a store of their own: the readers' are read-only.
async fn write<T: Send + 'static>(state: &Arc<AppState>, work: impl FnOnce(&Store) -> Result<T> + Send + 'static) -> Result<T, ApiError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || work(&Store::open(&db)?))
        .await
        .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("{error:#}")))
}

/// Looks for due schedules for as long as the server runs.
pub(crate) fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            if let Err(ApiError(_, error)) = tick(&state).await {
                tracing::warn!(error, "schedules");
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(state: &Arc<AppState>) -> Result<(), ApiError> {
    let at = now();
    let due: Vec<Schedule> =
        read(state, move |store| Ok(store.schedules()?.into_iter().filter(|schedule| schedule.enabled && schedule.next_run <= at).collect()))
            .await?;
    for schedule in due {
        if !run(state, schedule, true).await? {
            break;
        }
    }
    Ok(())
}

/// Starts the schedule's crawl and (`advance`) moves its next run on;
/// false (and nothing done) while another crawl runs.
async fn run(state: &Arc<AppState>, mut schedule: Schedule, advance: bool) -> Result<bool, ApiError> {
    let mut running = state.running.lock().await;
    if running.as_ref().is_some_and(Running::is_active) {
        return Ok(false);
    }
    if advance {
        schedule.next_run = next_after(schedule.next_run, schedule.every_hours, now());
    }
    let request: Result<StartRequest> = serde_json::from_value(schedule.request.clone()).map_err(Into::into);
    let started = match request.and_then(|request| crawls::build_config(&request)) {
        Ok(config) => crawls::start(state.db.clone(), config).await,
        Err(error) => Err(error),
    };
    match started {
        Ok(crawl) => {
            tracing::info!(schedule = schedule.id, crawl = crawl.id, "scheduled crawl started");
            schedule.last_crawl = Some(crawl.id);
            *running = Some(crawl.clone());
            drop(running);
            let saved = schedule.clone();
            write(state, move |store| store.save_schedule(&saved)).await?;
            tokio::spawn(report_when_done(state.clone(), schedule, crawl));
        }
        Err(error) => {
            drop(running);
            let saved = schedule.clone();
            write(state, move |store| store.save_schedule(&saved)).await?;
            send(state, &schedule, notify::failure(&start_url(&schedule), &format!("{error:#}"))).await;
        }
    }
    Ok(true)
}

fn start_url(schedule: &Schedule) -> String {
    schedule.request["start_url"].as_str().unwrap_or_default().to_string()
}

/// Sends to the schedule's chat, when it has one and the bot is set.
async fn send(state: &AppState, schedule: &Schedule, html: String) {
    let (Some(chat), Some(telegram)) = (&schedule.telegram_chat, &state.telegram) else { return };
    if let Err(error) = telegram.send(chat, &html).await {
        tracing::warn!(schedule = schedule.id, error = %format!("{error:#}"), "report not sent");
    }
}

async fn report_when_done(state: Arc<AppState>, schedule: Schedule, crawl: Running) {
    let mut phase = crawl.phase.clone();
    loop {
        let now = phase.borrow_and_update().clone();
        match now {
            Phase::Finished => break,
            Phase::Failed(error) => {
                send(&state, &schedule, notify::failure(&start_url(&schedule), &error)).await;
                return;
            }
            Phase::Crawling | Phase::Checking => {}
        }
        if phase.changed().await.is_err() {
            return;
        }
    }
    if schedule.telegram_chat.is_none() || state.telegram.is_none() {
        return;
    }
    let link = state.public_url.as_ref().map(|base| format!("{}/?crawl={}", base.trim_end_matches('/'), crawl.id));
    let id = crawl.id;
    match read(&state, move |store| notify::report(store, id, link.as_deref())).await {
        Ok(report) => send(&state, &schedule, report).await,
        Err(ApiError(_, error)) => tracing::warn!(error, "report not made"),
    }
}

#[derive(Serialize)]
pub(crate) struct Schedules {
    /// Reports can be sent: the bot's token is set.
    telegram: bool,
    schedules: Vec<Schedule>,
}

pub(crate) async fn list(State(state): State<Arc<AppState>>) -> ApiResult<Schedules> {
    let schedules = read(&state, |store| store.schedules()).await?;
    Ok(Json(Schedules { telegram: state.telegram.is_some(), schedules }))
}

#[derive(Serialize)]
pub(crate) struct Saved {
    id: i64,
}

/// Adds a schedule, or changes one (its id given). The request must make
/// a crawl now, not when it is due.
pub(crate) async fn save(State(state): State<Arc<AppState>>, Json(schedule): Json<Schedule>) -> ApiResult<Saved> {
    let request: StartRequest = serde_json::from_value(schedule.request.clone())
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("обход: {error}")))?;
    crawls::build_config(&request).map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("{error:#}")))?;
    if schedule.every_hours == 0 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "период: хотя бы час".into()));
    }
    let id = write(&state, move |store| store.save_schedule(&schedule)).await?;
    Ok(Json(Saved { id }))
}

pub(crate) async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    write(&state, move |store| store.delete_schedule(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Starts the schedule's crawl now; its next run stays.
pub(crate) async fn run_now(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Result<StatusCode, ApiError> {
    let schedule = read(&state, move |store| store.schedule(id))
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "нет такого расписания".into()))?;
    if !run(&state, schedule, false).await? {
        return Err(ApiError(StatusCode::CONFLICT, "обход уже идёт: остановите его или дождитесь конца".into()));
    }
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub(crate) struct Test {
    chat: String,
}

/// A message to the chat, to see that reports will reach it.
pub(crate) async fn test(State(state): State<Arc<AppState>>, Json(test): Json<Test>) -> Result<StatusCode, ApiError> {
    let telegram = state.telegram.as_ref().ok_or_else(|| {
        ApiError(StatusCode::BAD_REQUEST, "бот не задан: SEO_SPIDER_TELEGRAM_TOKEN в окружении сервера".into())
    })?;
    telegram
        .send(&test.chat, "seo-spider: отчёты об обходах будут приходить сюда")
        .await
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("{error:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_run_keeps_the_beat() {
        let day = 24;
        // Not due yet: unchanged.
        assert_eq!(next_after(1000, day, 500), 1000);
        // Due: a day on; days missed are skipped, the time of day kept.
        assert_eq!(next_after(1000, day, 1000), 1000 + 86_400);
        assert_eq!(next_after(1000, day, 1000 + 3 * 86_400 + 5), 1000 + 4 * 86_400);
        assert_eq!(next_after(0, 0, 10), 3600);
    }
}
