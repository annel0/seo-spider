//! Two crawls of a site compared: the store's `compare`, the older crawl
//! first, with the issues named and the lists cut for the browser (the CSV
//! has them whole).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use anyhow::Result;
use seo_crawl::{CrawlConfig, FetchLevel};
use seo_store::{CrawlDiff, DiffChange, DiffIssue, DiffUrl, Store};
use serde::{Deserialize, Serialize};

use crate::{read, ApiError, ApiResult, AppState};

/// Rows of each list (of each field, for changes) the browser gets.
const SHOWN: usize = 500;

#[derive(Deserialize)]
pub(crate) struct With {
    /// The crawl to compare with.
    with: i64,
}

#[derive(Serialize)]
pub(crate) struct Shift {
    code: String,
    title: Option<&'static str>,
    severity: Option<&'static str>,
    before: i64,
    after: i64,
    appeared: i64,
    fixed: i64,
}

/// How long each list was before it was cut.
#[derive(Serialize)]
pub(crate) struct Totals {
    added: usize,
    removed: usize,
    /// Per field.
    changed: BTreeMap<&'static str, usize>,
    /// URLs with any change.
    changed_urls: usize,
    new_issues: usize,
    fixed_issues: usize,
}

#[derive(Serialize)]
pub(crate) struct Comparison {
    old: i64,
    new: i64,
    /// How the crawls' settings differ where it shows in the comparison.
    notes: Vec<String>,
    totals: Totals,
    added: Vec<DiffUrl>,
    removed: Vec<DiffUrl>,
    changed: Vec<DiffChange>,
    new_issues: Vec<DiffIssue>,
    fixed_issues: Vec<DiffIssue>,
    shifts: Vec<Shift>,
}

/// The two crawls as (older, newer).
fn ordered(id: i64, with: i64) -> (i64, i64) {
    (id.min(with), id.max(with))
}

fn cut<T>(mut list: Vec<T>) -> Vec<T> {
    list.truncate(SHOWN);
    list
}

/// What a crawl read of its pages.
struct Reads {
    head: bool,
    headings: bool,
    text: bool,
    render: bool,
}

fn reads(config: &CrawlConfig) -> Reads {
    let full = matches!(config.fetch, FetchLevel::Full);
    Reads {
        head: !matches!(config.fetch, FetchLevel::Status),
        headings: full && config.extract.headings,
        text: full && config.extract.text,
        render: config.render.is_some(),
    }
}

/// The two crawls compared over the fields both read, with notes on
/// settings that make a difference by themselves.
pub(crate) fn diff(store: &Store, old: i64, new: i64) -> Result<(CrawlDiff, Vec<String>)> {
    let config = |id: i64| -> Result<CrawlConfig> { Ok(serde_json::from_str(&store.crawl_config(id)?).unwrap_or_default()) };
    let (before, after) = (config(old)?, config(new)?);
    let (was, is) = (reads(&before), reads(&after));
    let mut fields = Vec::new();
    if was.head && is.head {
        fields.extend(["title", "description", "canonical", "robots"]);
    }
    if was.headings && is.headings {
        fields.push("h1");
    }
    if was.text && is.text {
        fields.push("words");
    }
    let mut notes = Vec::new();
    let skipped: Vec<&str> = seo_store::COMPARED_FIELDS.iter().filter(|field| !fields.contains(field)).map(|field| field_title(field)).collect();
    if !skipped.is_empty() {
        notes.push(format!("Не сравниваются: {}. Один из обходов их не читал", skipped.join(", ")));
    }
    if was.render != is.render {
        let rendered = if was.render { old } else { new };
        notes.push(format!("JavaScript выполнялся только в #{rendered}: часть отличий от этого"));
    }
    if serde_json::to_value(&before.checks).ok() != serde_json::to_value(&after.checks).ok() {
        notes.push("Проверки настроены по-разному: часть проблем от этого".into());
    }
    if before.max_urls != after.max_urls {
        notes.push(format!("Разный лимит URL: {} и {}", before.max_urls, after.max_urls));
    }
    Ok((store.compare(old, new, &fields)?, notes))
}

fn comparison(diff: CrawlDiff, notes: Vec<String>) -> Comparison {
    let mut changed_per_field: BTreeMap<&'static str, usize> = BTreeMap::new();
    for change in &diff.changed {
        *changed_per_field.entry(change.field).or_default() += 1;
    }
    let totals = Totals {
        added: diff.added.len(),
        removed: diff.removed.len(),
        changed_urls: diff.changed.iter().map(|change| change.id).collect::<HashSet<_>>().len(),
        changed: changed_per_field,
        new_issues: diff.new_issues.len(),
        fixed_issues: diff.fixed_issues.len(),
    };
    let mut shown: BTreeMap<&'static str, usize> = BTreeMap::new();
    let changed = diff
        .changed
        .into_iter()
        .filter(|change| {
            let count = shown.entry(change.field).or_default();
            *count += 1;
            *count <= SHOWN
        })
        .collect();
    let shifts = diff
        .shifts
        .into_iter()
        .map(|shift| {
            let check = seo_checks::find(&shift.code);
            Shift {
                title: check.map(|check| check.title),
                severity: check.map(|check| check.severity.as_str()),
                code: shift.code,
                before: shift.before,
                after: shift.after,
                appeared: shift.appeared,
                fixed: shift.fixed,
            }
        })
        .collect();
    Comparison {
        old: diff.old,
        new: diff.new,
        notes,
        totals,
        added: cut(diff.added),
        removed: cut(diff.removed),
        changed,
        new_issues: cut(diff.new_issues),
        fixed_issues: cut(diff.fixed_issues),
        shifts,
    }
}

/// What changed between the crawl and another one, older to newer.
pub(crate) async fn compare(State(state): State<Arc<AppState>>, Path(id): Path<i64>, Query(other): Query<With>) -> ApiResult<Comparison> {
    let (old, new) = ordered(id, other.with);
    let (diff, notes) = read(&state, move |store| diff(store, old, new)).await?;
    Ok(Json(comparison(diff, notes)))
}

/// A changed field's name in the CSV.
pub(crate) fn field_title(field: &str) -> &str {
    match field {
        "status" => "Код ответа",
        "title" => "Title",
        "description" => "Description",
        "h1" => "H1",
        "canonical" => "Canonical",
        "robots" => "Meta robots",
        "words" => "Слов",
        other => other,
    }
}

fn issue_title(code: &str) -> &str {
    seo_checks::find(code).map_or(code, |check| check.title)
}

/// The whole comparison as CSV: one row per new or gone URL, changed
/// field, and issue a URL got or lost.
pub(crate) async fn compare_csv(State(state): State<Arc<AppState>>, Path(id): Path<i64>, Query(other): Query<With>) -> Result<Response, ApiError> {
    let (old, new) = ordered(id, other.with);
    let bytes = read(&state, move |store| {
        let (diff, _) = diff(store, old, new)?;
        let mut writer = csv::Writer::from_writer(b"\xEF\xBB\xBF".to_vec());
        writer.write_record(["Что", "URL", "Поле", "Было", "Стало"])?;
        for url in &diff.added {
            writer.write_record(["Новый URL", &url.url, "Код ответа", "", &url.status.to_string()])?;
        }
        for url in &diff.removed {
            writer.write_record(["Исчез", &url.url, "Код ответа", &url.status.to_string(), ""])?;
        }
        for change in &diff.changed {
            let (before, after) = (change.before.as_deref().unwrap_or(""), change.after.as_deref().unwrap_or(""));
            writer.write_record(["Изменение", &change.url, field_title(change.field), before, after])?;
        }
        for issue in &diff.new_issues {
            writer.write_record(["Новая проблема", &issue.url, issue_title(&issue.code), "", "есть"])?;
        }
        for issue in &diff.fixed_issues {
            writer.write_record(["Исправлено", &issue.url, issue_title(&issue.code), "есть", ""])?;
        }
        writer.into_inner().map_err(|error| anyhow::anyhow!("{error}"))
    })
    .await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"compare-{old}-{new}.csv\"")),
        ],
        bytes,
    )
        .into_response())
}
