//! Tabs of a crawl, streamed row by row into CSV files or one XLSX workbook:
//! facts are read a chunk at a time and turned into rows in parallel, so
//! memory does not grow with the crawl.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::params;
use rustc_hash::FxHashMap;
use seo_page::{HttpInfo, PageFacts};
use seo_store::{Store, StoredUrl};

pub enum Cell {
    Text(String),
    Int(i64),
    Bool(bool),
    Empty,
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Cell::Text(text.to_string())
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Cell::Text(text)
    }
}

impl From<Option<&str>> for Cell {
    fn from(text: Option<&str>) -> Self {
        text.map_or(Cell::Empty, Cell::from)
    }
}

impl From<Option<String>> for Cell {
    fn from(text: Option<String>) -> Self {
        text.map_or(Cell::Empty, Cell::Text)
    }
}

impl From<i64> for Cell {
    fn from(value: i64) -> Self {
        Cell::Int(value)
    }
}

impl From<usize> for Cell {
    fn from(value: usize) -> Self {
        Cell::Int(value as i64)
    }
}

impl From<Option<u32>> for Cell {
    fn from(value: Option<u32>) -> Self {
        value.map_or(Cell::Empty, |value| Cell::Int(value.into()))
    }
}

impl From<bool> for Cell {
    fn from(value: bool) -> Self {
        Cell::Bool(value)
    }
}

/// Where tables go, one row at a time.
pub trait Sink {
    fn begin(&mut self, name: &'static str, headers: &[&'static str]) -> Result<()>;
    fn row(&mut self, cells: &[Cell]) -> Result<()>;
    /// Ends the table; returns its rows written.
    fn end(&mut self) -> Result<usize>;
    fn finish(self: Box<Self>) -> Result<()>;
}

/// XLSX holds at most this many rows per sheet.
const MAX_ROWS: usize = 1_048_575;
/// URLs read and turned into rows at a time.
const CHUNK: usize = 2048;

/// Writes every tab of the crawl; returns (tab, rows).
pub fn export(store: &Store, crawl_id: i64, sink: &mut dyn Sink) -> Result<Vec<(&'static str, usize, f64)>> {
    let mut written = Vec::new();
    let mut timed = |tab: &'static str, rows: Result<usize>, started: std::time::Instant| -> Result<()> {
        written.push((tab, rows?, started.elapsed().as_secs_f64()));
        Ok(())
    };
    let started = std::time::Instant::now();
    let inlinks = inlinks(store, crawl_id)?;
    timed("(inlinks)", Ok(inlinks.len()), started)?;
    let started = std::time::Instant::now();
    timed("summary", summary(store, crawl_id, sink), started)?;
    let started = std::time::Instant::now();
    timed("internal", internal(store, crawl_id, &inlinks, sink), started)?;
    let started = std::time::Instant::now();
    timed("external", external(store, crawl_id, &inlinks, sink), started)?;
    let started = std::time::Instant::now();
    timed("redirects", redirects(store, crawl_id, sink), started)?;
    let started = std::time::Instant::now();
    timed("javascript", javascript(store, crawl_id, sink), started)?;
    let started = std::time::Instant::now();
    timed("links", links(store, crawl_id, sink), started)?;
    let started = std::time::Instant::now();
    timed("issues", issues(store, crawl_id, sink), started)?;
    Ok(written)
}

/// Link counts into each URL, (links, linking pages), from the document
/// each page was judged by: the rendered one when the page was rendered,
/// the raw one otherwise. Read from the facts, which hold every link
/// whether or not the crawl kept link rows.
fn inlinks(store: &Store, crawl_id: i64) -> Result<FxHashMap<String, (i64, i64)>> {
    let mut counts: FxHashMap<String, (i64, i64)> = FxHashMap::default();
    store.for_each_url_chunk(crawl_id, CHUNK, |chunk| {
        // Each page's targets with how often it links to each, so a page
        // counts once among a target's linking pages.
        let pages: Vec<FxHashMap<String, i64>> = chunk
            .into_par_iter()
            .map(|encoded| -> Result<_> {
                let url = encoded.decode()?;
                let mut targets: FxHashMap<String, i64> = FxHashMap::default();
                for link in url.facts().map_or(&[][..], |facts| &facts.links) {
                    if !link.href.is_empty() {
                        *targets.entry(link.href.clone()).or_default() += 1;
                    }
                }
                Ok(targets)
            })
            .collect::<Result<_>>()?;
        for (target, links) in pages.into_iter().flatten() {
            let entry = counts.entry(target).or_default();
            entry.0 += links;
            entry.1 += 1;
        }
        Ok(())
    })?;
    Ok(counts)
}

fn chars(text: Option<&str>) -> Cell {
    text.map_or(Cell::Empty, |text| Cell::Int(text.chars().count() as i64))
}

fn x_robots(http: &HttpInfo) -> Option<String> {
    let values: Vec<_> = http.header_values("x-robots-tag").collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// ("Indexable" | "Non-Indexable", reason).
fn indexability(url: &StoredUrl, facts: Option<&PageFacts>) -> (&'static str, &'static str) {
    let status = url.http.status;
    let reason = if url.blocked_by_robots {
        "Blocked by robots.txt"
    } else if status == 0 {
        "No Response"
    } else if (300..400).contains(&status) {
        "Redirected"
    } else if (400..500).contains(&status) {
        "Client Error"
    } else if status >= 500 {
        "Server Error"
    } else if facts.is_some_and(|facts| facts.robots_tokens().any(|token| token == "noindex" || token == "none"))
        || x_robots(&url.http).is_some_and(|value| value.to_ascii_lowercase().contains("noindex"))
    {
        "noindex"
    } else if facts.and_then(|facts| facts.canonicals.first()).is_some_and(|canonical| *canonical != url.http.url) {
        "Canonicalised"
    } else {
        return ("Indexable", "");
    };
    ("Non-Indexable", reason)
}

/// Streams the crawl's URLs a chunk at a time, decoding facts and building
/// rows in parallel; `row` returns `None` for URLs the tab skips.
fn url_rows(
    store: &Store,
    crawl_id: i64,
    sink: &mut dyn Sink,
    row: impl Fn(&StoredUrl) -> Option<Vec<Cell>> + Sync,
) -> Result<()> {
    url_row_groups(store, crawl_id, sink, |url| row(url).into_iter().collect())
}

/// `url_rows` for tabs with any number of rows per URL.
fn url_row_groups(
    store: &Store,
    crawl_id: i64,
    sink: &mut dyn Sink,
    rows: impl Fn(&StoredUrl) -> Vec<Vec<Cell>> + Sync,
) -> Result<()> {
    store.for_each_url_chunk(crawl_id, CHUNK, |chunk| {
        let groups: Vec<Vec<Vec<Cell>>> = chunk
            .into_par_iter()
            .map(|encoded| -> Result<_> { Ok(rows(&encoded.decode()?)) })
            .collect::<Result<_>>()?;
        for cells in groups.into_iter().flatten() {
            sink.row(&cells)?;
        }
        Ok(())
    })
}

fn internal(store: &Store, crawl_id: i64, inlinks: &FxHashMap<String, (i64, i64)>, sink: &mut dyn Sink) -> Result<usize> {
    sink.begin(
        "internal",
        &[
            "Address", "Content Type", "Status Code", "Status", "Indexability", "Indexability Status",
            "Title 1", "Title 1 Length", "Meta Description 1", "Meta Description 1 Length",
            "H1-1", "H1-1 Length", "H1 Count", "H2-1", "Meta Robots 1", "X-Robots-Tag 1",
            "Canonical Link Element 1", "Meta Refresh 1", "Language", "Size (bytes)", "Word Count",
            "Text Hash", "Crawl Depth", "Inlinks", "Unique Inlinks", "Outlinks", "Response Time (ms)",
            "Redirect URL", "In Sitemap", "Rendered",
        ],
    )?;
    url_rows(store, crawl_id, sink, |url| {
        if !url.internal {
            return None;
        }
        let facts = url.facts();
        let (indexable, reason) = indexability(url, facts);
        let (links_in, unique_in) = inlinks.get(&url.http.url).copied().unwrap_or((0, 0));
        let title = facts.and_then(PageFacts::title);
        let description = facts.and_then(PageFacts::description);
        let h1 = facts.and_then(|facts| facts.h1().next());
        Some(vec![
            url.http.url.as_str().into(),
            url.http.content_type.as_deref().into(),
            Cell::Int(url.http.status.into()),
            url.http.error.as_deref().into(),
            indexable.into(),
            reason.into(),
            title.into(),
            chars(title),
            description.into(),
            chars(description),
            h1.into(),
            chars(h1),
            facts.filter(|facts| facts.extracted.headings).map_or(Cell::Empty, |facts| facts.h1().count().into()),
            facts.and_then(|facts| facts.h2().next()).into(),
            facts.and_then(|facts| facts.meta_robots.first().map(String::as_str)).into(),
            x_robots(&url.http).into(),
            facts.and_then(|facts| facts.canonicals.first().map(String::as_str)).into(),
            facts
                .and_then(|facts| facts.meta_refresh.as_ref())
                .map(|(delay, target)| format!("{delay}; {}", target.as_deref().unwrap_or("")))
                .into(),
            facts.and_then(|facts| facts.lang.as_deref()).into(),
            Cell::Int(url.http.size as i64),
            facts.filter(|facts| facts.extracted.text).map_or(Cell::Empty, |facts| facts.word_count.into()),
            facts
                .filter(|facts| facts.extracted.text)
                .map_or(Cell::Empty, |facts| format!("{:016x}", facts.text_hash).into()),
            url.depth.into(),
            links_in.into(),
            unique_in.into(),
            facts.filter(|facts| facts.extracted.links).map_or(Cell::Empty, |facts| facts.links.len().into()),
            Cell::Int(url.http.ttfb_ms.into()),
            url.http.location.as_deref().into(),
            url.in_sitemap.into(),
            url.rendered.is_some().into(),
        ])
    })?;
    sink.end()
}

fn external(store: &Store, crawl_id: i64, inlinks: &FxHashMap<String, (i64, i64)>, sink: &mut dyn Sink) -> Result<usize> {
    sink.begin(
        "external",
        &["Address", "Content Type", "Status Code", "Status", "Redirect URL", "Response Time (ms)", "Inlinks"],
    )?;
    let mut select = store.connection().prepare(
        "SELECT url, content_type, status, error, location, ttfb_ms FROM urls
         WHERE crawl_id = ?1 AND internal = 0 ORDER BY id",
    )?;
    let mut rows = select.query(params![crawl_id])?;
    while let Some(row) = rows.next()? {
        let url: String = row.get(0)?;
        let unique_in = inlinks.get(&url).map_or(0, |counts| counts.1);
        sink.row(&[
            url.into(),
            row.get::<_, Option<String>>(1)?.into(),
            Cell::Int(row.get(2)?),
            row.get::<_, Option<String>>(3)?.into(),
            row.get::<_, Option<String>>(4)?.into(),
            Cell::Int(row.get(5)?),
            unique_in.into(),
        ])?;
    }
    sink.end()
}

/// One row per redirect chain, from a URL no other redirect points at, then
/// one per loop that has no such start.
fn redirects(store: &Store, crawl_id: i64, sink: &mut dyn Sink) -> Result<usize> {
    sink.begin("redirects", &["Address", "Hops", "Final Address", "Final Status Code", "Loop", "Chain"])?;
    // (url, status, redirect target) of every URL: small, unlike facts.
    let mut select = store
        .connection()
        .prepare("SELECT url, status, location FROM urls WHERE crawl_id = ?1 ORDER BY id")?;
    let urls: Vec<(String, u16, Option<String>)> = select
        .query_map(params![crawl_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let redirect = |status: u16, location: &Option<String>| (300..400).contains(&status) && location.is_some();
    let by_url: HashMap<&str, (u16, Option<&str>)> = urls
        .iter()
        .map(|(url, status, location)| {
            (url.as_str(), (*status, location.as_deref().filter(|_| redirect(*status, location))))
        })
        .collect();
    let targets: HashSet<&str> = by_url.values().filter_map(|(_, next)| *next).collect();
    let mut walked = HashSet::new();
    let starts = urls.iter().filter(|(url, status, location)| redirect(*status, location) && !targets.contains(url.as_str()));
    for (url, _, _) in starts {
        sink.row(&chain(url, &by_url, &mut walked))?;
    }
    for (url, status, location) in &urls {
        if redirect(*status, location) && !walked.contains(url.as_str()) {
            sink.row(&chain(url, &by_url, &mut walked))?;
        }
    }
    sink.end()
}

fn chain<'a>(start: &'a str, by_url: &HashMap<&'a str, (u16, Option<&'a str>)>, walked: &mut HashSet<&'a str>) -> Vec<Cell> {
    let mut status = by_url[start].0;
    let mut hops = vec![format!("{status} {start}")];
    let mut seen = HashSet::from([start]);
    walked.insert(start);
    let mut at = start;
    let mut looped = false;
    let mut redirects = 0usize;
    while let Some(next) = by_url.get(at).and_then(|(_, next)| *next) {
        redirects += 1;
        if !seen.insert(next) {
            looped = true;
            hops.push(format!("loop to {next}"));
            break;
        }
        let Some((&url, &(next_status, _))) = by_url.get_key_value(next) else {
            hops.push(format!("not crawled {next}"));
            break;
        };
        walked.insert(url);
        hops.push(format!("{next_status} {url}"));
        at = url;
        status = next_status;
        if hops.len() > 20 {
            break;
        }
    }
    vec![
        start.into(),
        redirects.into(),
        at.into(),
        Cell::Int(status.into()),
        looped.into(),
        hops.join(" → ").into(),
    ]
}

/// What a render left, for the JavaScript tab.
struct RenderInfo {
    error: Option<String>,
    settled: bool,
    total_ms: i64,
    console_errors: usize,
    failed_requests: usize,
}

/// Raw HTML against the rendered DOM for every rendered page.
fn javascript(store: &Store, crawl_id: i64, sink: &mut dyn Sink) -> Result<usize> {
    sink.begin(
        "javascript",
        &[
            "Address", "Render Error", "Settled", "Render Time (ms)", "Console Errors", "Failed Requests",
            "Title Raw", "Title Rendered", "H1 Raw", "H1 Rendered", "Canonical Raw", "Canonical Rendered",
            "Meta Robots Raw", "Meta Robots Rendered", "Word Count Raw", "Word Count Rendered",
            "Links Raw", "Links Rendered", "Links Only In Rendered",
        ],
    )?;
    let mut select = store.connection().prepare(
        "SELECT r.url_id, r.error, r.settled, r.total_ms, r.console_errors, r.resources
         FROM renders r JOIN urls u ON u.id = r.url_id WHERE u.crawl_id = ?1",
    )?;
    let renders: HashMap<i64, RenderInfo> = select
        .query_map(params![crawl_id], |row| {
            let console: String = row.get(4)?;
            let resources: String = row.get(5)?;
            Ok((
                row.get(0)?,
                RenderInfo {
                    error: row.get(1)?,
                    settled: row.get(2)?,
                    total_ms: row.get(3)?,
                    console_errors: serde_json::from_str::<Vec<String>>(&console).map_or(0, |errors| errors.len()),
                    failed_requests: serde_json::from_str::<Vec<serde_json::Value>>(&resources).map_or(0, |resources| {
                        resources
                            .iter()
                            .filter(|resource| resource["status"].as_u64().is_none_or(|status| status == 0 || status >= 400))
                            .count()
                    }),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if renders.is_empty() {
        return sink.end();
    }
    url_rows(store, crawl_id, sink, |url| {
        let render = renders.get(&url.id)?;
        let raw = url.raw.as_ref()?;
        let rendered = url.rendered.as_ref();
        let raw_links: HashSet<&str> = raw.links.iter().map(|link| link.href.as_str()).collect();
        let only_rendered = rendered.map_or(0, |facts| {
            facts
                .links
                .iter()
                .map(|link| link.href.as_str())
                .filter(|href| !raw_links.contains(href))
                .collect::<HashSet<_>>()
                .len()
        });
        Some(vec![
            url.http.url.as_str().into(),
            render.error.clone().into(),
            render.settled.into(),
            render.total_ms.into(),
            render.console_errors.into(),
            render.failed_requests.into(),
            raw.title().into(),
            rendered.and_then(PageFacts::title).into(),
            raw.h1().next().into(),
            rendered.and_then(|facts| facts.h1().next()).into(),
            raw.canonicals.first().map(String::as_str).into(),
            rendered.and_then(|facts| facts.canonicals.first().map(String::as_str)).into(),
            raw.meta_robots.first().map(String::as_str).into(),
            rendered.and_then(|facts| facts.meta_robots.first().map(String::as_str)).into(),
            raw.word_count.into(),
            rendered.map_or(Cell::Empty, |facts| facts.word_count.into()),
            raw.links.len().into(),
            rendered.map_or(Cell::Empty, |facts| facts.links.len().into()),
            only_rendered.into(),
        ])
    })?;
    sink.end()
}

/// Every link of every page, from the document the page was judged by,
/// with the status of its target when the crawl requested it.
fn links(store: &Store, crawl_id: i64, sink: &mut dyn Sink) -> Result<usize> {
    sink.begin("links", &["From", "To", "To Status Code", "Anchor", "Rel", "Nofollow", "Position", "Image", "Document"])?;
    let statuses: FxHashMap<String, i64> = store
        .connection()
        .prepare("SELECT url, status FROM urls WHERE crawl_id = ?1")?
        .query_map(params![crawl_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    url_row_groups(store, crawl_id, sink, |url| {
        let Some(facts) = url.facts() else { return Vec::new() };
        facts
            .links
            .iter()
            .map(|link| {
                vec![
                    Cell::from(url.http.url.as_str()),
                    Cell::from(link.href.as_str()),
                    statuses.get(&link.href).map_or(Cell::Empty, |status| Cell::Int(*status)),
                    Cell::from(link.anchor.as_str()),
                    Cell::Text(link.rel.join(" ")),
                    Cell::Bool(link.nofollow),
                    Cell::from(link.position.as_str()),
                    Cell::Bool(link.image),
                    Cell::from(facts.source.as_str()),
                ]
            })
            .collect()
    })?;
    sink.end()
}

fn issues(store: &Store, crawl_id: i64, sink: &mut dyn Sink) -> Result<usize> {
    sink.begin("issues", &["Address", "Severity", "Category", "Issue", "Code", "Detail"])?;
    let mut select = store.connection().prepare(
        "SELECT u.url, i.code, i.detail FROM issues i JOIN urls u ON u.id = i.url_id
         WHERE i.crawl_id = ?1 ORDER BY u.id",
    )?;
    let mut rows = select
        .query_map(params![crawl_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rank = |code: &str| seo_checks::find(code).map(|check| (check.severity, check.category));
    rows.sort_by(|a, b| rank(&a.1).cmp(&rank(&b.1)).then_with(|| a.1.cmp(&b.1)));
    for (url, code, detail) in rows {
        let check = seo_checks::find(&code);
        sink.row(&[
            url.into(),
            check.map(|check| check.severity.as_str()).into(),
            check.map(|check| check.category).into(),
            check.map(|check| check.title).into(),
            code.into(),
            detail.into(),
        ])?;
    }
    sink.end()
}

/// How many pages each check flagged, most severe first.
fn summary(store: &Store, crawl_id: i64, sink: &mut dyn Sink) -> Result<usize> {
    sink.begin("summary", &["Severity", "Category", "Issue", "Code", "URLs", "Description"])?;
    let mut select = store
        .connection()
        .prepare("SELECT code, count(DISTINCT url_id) FROM issues WHERE crawl_id = ?1 GROUP BY code")?;
    let counts: HashMap<String, i64> = select
        .query_map(params![crawl_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut checks: Vec<_> = seo_checks::CATALOG.iter().filter(|check| counts.contains_key(check.code)).collect();
    checks.sort_by_key(|check| (check.severity, check.category, std::cmp::Reverse(counts[check.code])));
    for check in checks {
        sink.row(&[
            check.severity.as_str().into(),
            check.category.into(),
            check.title.into(),
            check.code.into(),
            counts[check.code].into(),
            check.description.into(),
        ])?;
    }
    sink.end()
}

/// One CSV file per tab in a directory.
pub struct CsvSink {
    dir: PathBuf,
    writer: Option<csv::Writer<std::fs::File>>,
    rows: usize,
}

impl CsvSink {
    pub fn new(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(CsvSink { dir: dir.to_path_buf(), writer: None, rows: 0 })
    }
}

impl Sink for CsvSink {
    fn begin(&mut self, name: &'static str, headers: &[&'static str]) -> Result<()> {
        let mut writer = csv::Writer::from_path(self.dir.join(format!("{name}.csv")))?;
        writer.write_record(headers)?;
        self.writer = Some(writer);
        self.rows = 0;
        Ok(())
    }

    fn row(&mut self, cells: &[Cell]) -> Result<()> {
        let writer = self.writer.as_mut().context("row before begin")?;
        writer.write_record(cells.iter().map(|cell| -> std::borrow::Cow<'_, [u8]> {
            match cell {
                Cell::Text(text) => text.as_bytes().into(),
                Cell::Int(value) => value.to_string().into_bytes().into(),
                Cell::Bool(value) => if *value { b"true".as_slice() } else { b"false".as_slice() }.into(),
                Cell::Empty => b"".as_slice().into(),
            }
        }))?;
        self.rows += 1;
        Ok(())
    }

    fn end(&mut self) -> Result<usize> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        Ok(self.rows)
    }

    fn finish(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

/// One XLSX workbook, a sheet per tab, written in constant memory.
pub struct XlsxSink {
    path: PathBuf,
    workbook: rust_xlsxwriter::Workbook,
    bold: rust_xlsxwriter::Format,
    sheet: usize,
    sheets: usize,
    columns: u16,
    rows: usize,
}

impl XlsxSink {
    pub fn new(path: &Path) -> Self {
        XlsxSink {
            path: path.to_path_buf(),
            workbook: rust_xlsxwriter::Workbook::new(),
            bold: rust_xlsxwriter::Format::new().set_bold(),
            sheet: 0,
            sheets: 0,
            columns: 0,
            rows: 0,
        }
    }

    fn sheet(&mut self) -> Result<&mut rust_xlsxwriter::Worksheet> {
        Ok(self.workbook.worksheet_from_index(self.sheet)?)
    }
}

impl Sink for XlsxSink {
    fn begin(&mut self, name: &'static str, headers: &[&'static str]) -> Result<()> {
        let bold = self.bold.clone();
        let sheet = self.workbook.add_worksheet_with_constant_memory();
        sheet.set_name(name)?;
        for (col, header) in headers.iter().enumerate() {
            sheet.write_string_with_format(0, col as u16, *header, &bold)?;
        }
        sheet.set_freeze_panes(1, 0)?;
        self.sheet = self.sheets;
        self.sheets += 1;
        self.columns = headers.len() as u16;
        self.rows = 0;
        Ok(())
    }

    fn row(&mut self, cells: &[Cell]) -> Result<()> {
        /// Longest text a cell holds.
        const MAX_CELL: usize = 32_767;
        if self.rows >= MAX_ROWS {
            return Ok(());
        }
        self.rows += 1;
        let row = self.rows as u32;
        let sheet = self.sheet()?;
        for (col, cell) in cells.iter().enumerate() {
            let col = col as u16;
            match cell {
                Cell::Text(text) if text.len() > MAX_CELL => {
                    let cut: String = text.chars().take(MAX_CELL / 4).collect();
                    sheet.write_string(row, col, cut)?;
                }
                Cell::Text(text) => {
                    sheet.write_string(row, col, text)?;
                }
                Cell::Int(value) => {
                    sheet.write_number(row, col, *value as f64)?;
                }
                Cell::Bool(value) => {
                    sheet.write_boolean(row, col, *value)?;
                }
                Cell::Empty => {}
            }
        }
        Ok(())
    }

    fn end(&mut self) -> Result<usize> {
        let (rows, columns) = (self.rows as u32, self.columns);
        if columns > 0 {
            self.sheet()?.autofilter(0, 0, rows, columns - 1)?;
        }
        Ok(self.rows)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.workbook.save(&self.path)?;
        Ok(())
    }
}
