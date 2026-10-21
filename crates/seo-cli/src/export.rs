//! Tabs of a crawl as tables, written as CSV files or one XLSX workbook.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;
use seo_page::PageFacts;
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

pub struct Table {
    pub name: &'static str,
    pub headers: Vec<&'static str>,
    pub rows: Vec<Vec<Cell>>,
}

/// XLSX holds at most this many rows per sheet.
const MAX_ROWS: usize = 1_048_575;

pub fn tables(store: &Store, crawl_id: i64) -> Result<Vec<Table>> {
    let urls = store.urls(crawl_id)?;
    let by_url: HashMap<&str, &StoredUrl> = urls.iter().map(|url| (url.http.url.as_str(), url)).collect();
    let inlinks = inlinks(store, crawl_id)?;
    Ok(vec![
        summary(store, crawl_id)?,
        internal(&urls, &inlinks),
        external(&urls, &inlinks),
        redirects(&urls, &by_url),
        javascript(store, &urls)?,
        links(store, crawl_id)?,
        issues(store, crawl_id)?,
    ])
}

/// Link counts into each URL from the document each page was judged by:
/// the rendered one when the page was rendered, the raw one otherwise.
fn inlinks(store: &Store, crawl_id: i64) -> Result<HashMap<String, (i64, i64)>> {
    let mut select = store.connection().prepare(
        "SELECT l.to_url, count(*), count(DISTINCT l.from_url_id)
         FROM links l
         WHERE l.crawl_id = ?1 AND l.source = (
             CASE WHEN EXISTS (SELECT 1 FROM facts f WHERE f.url_id = l.from_url_id AND f.source = 'rendered')
             THEN 'rendered' ELSE 'raw' END)
         GROUP BY l.to_url",
    )?;
    let rows = select.query_map(params![crawl_id], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn chars(text: Option<&str>) -> Cell {
    text.map_or(Cell::Empty, |text| Cell::Int(text.chars().count() as i64))
}

fn x_robots(url: &StoredUrl) -> Option<String> {
    let values: Vec<_> = url.http.header_values("x-robots-tag").collect();
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
        || x_robots(url).is_some_and(|value| value.to_ascii_lowercase().contains("noindex"))
    {
        "noindex"
    } else if facts.and_then(|facts| facts.canonicals.first()).is_some_and(|canonical| *canonical != url.http.url) {
        "Canonicalised"
    } else {
        return ("Indexable", "");
    };
    ("Non-Indexable", reason)
}

fn internal(urls: &[StoredUrl], inlinks: &HashMap<String, (i64, i64)>) -> Table {
    let headers = vec![
        "Address", "Content Type", "Status Code", "Status", "Indexability", "Indexability Status",
        "Title 1", "Title 1 Length", "Meta Description 1", "Meta Description 1 Length",
        "H1-1", "H1-1 Length", "H1 Count", "H2-1", "Meta Robots 1", "X-Robots-Tag 1",
        "Canonical Link Element 1", "Meta Refresh 1", "Language", "Size (bytes)", "Word Count",
        "Text Hash", "Crawl Depth", "Inlinks", "Unique Inlinks", "Outlinks", "Response Time (ms)",
        "Redirect URL", "In Sitemap", "Rendered",
    ];
    let rows = urls
        .iter()
        .filter(|url| url.internal)
        .map(|url| {
            let facts = url.facts();
            let (indexable, reason) = indexability(url, facts);
            let (links_in, unique_in) = inlinks.get(&url.http.url).copied().unwrap_or((0, 0));
            let title = facts.and_then(PageFacts::title);
            let description = facts.and_then(PageFacts::description);
            let h1 = facts.and_then(|facts| facts.h1().next());
            vec![
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
                x_robots(url).into(),
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
            ]
        })
        .collect();
    Table { name: "internal", headers, rows }
}

fn external(urls: &[StoredUrl], inlinks: &HashMap<String, (i64, i64)>) -> Table {
    let headers = vec!["Address", "Content Type", "Status Code", "Status", "Redirect URL", "Response Time (ms)", "Inlinks"];
    let rows = urls
        .iter()
        .filter(|url| !url.internal)
        .map(|url| {
            vec![
                url.http.url.as_str().into(),
                url.http.content_type.as_deref().into(),
                Cell::Int(url.http.status.into()),
                url.http.error.as_deref().into(),
                url.http.location.as_deref().into(),
                Cell::Int(url.http.ttfb_ms.into()),
                inlinks.get(&url.http.url).map_or(0, |counts| counts.1).into(),
            ]
        })
        .collect();
    Table { name: "external", headers, rows }
}

/// One row per redirect chain, from a URL no other redirect points at, then
/// one per loop that has no such start.
fn redirects(urls: &[StoredUrl], by_url: &HashMap<&str, &StoredUrl>) -> Table {
    let headers = vec!["Address", "Hops", "Final Address", "Final Status Code", "Loop", "Chain"];
    let is_redirect = |url: &&StoredUrl| url.http.is_redirect();
    let targets: HashSet<&str> = urls.iter().filter(is_redirect).filter_map(|url| url.http.location.as_deref()).collect();
    let starts = urls.iter().filter(is_redirect).filter(|url| !targets.contains(url.http.url.as_str()));
    let mut walked = HashSet::new();
    let mut rows: Vec<Vec<Cell>> = starts.map(|start| chain(start, by_url, &mut walked)).collect();
    for url in urls.iter().filter(is_redirect) {
        if !walked.contains(url.http.url.as_str()) {
            rows.push(chain(url, by_url, &mut walked));
        }
    }
    Table { name: "redirects", headers, rows }
}

fn chain<'a>(start: &'a StoredUrl, by_url: &HashMap<&str, &'a StoredUrl>, walked: &mut HashSet<&'a str>) -> Vec<Cell> {
    let mut hops = vec![format!("{} {}", start.http.status, start.http.url)];
    let mut seen = HashSet::from([start.http.url.as_str()]);
    walked.insert(start.http.url.as_str());
    let mut at = start;
    let mut looped = false;
    let mut redirects = 0usize;
    while let Some(next) = at.http.location.as_deref().filter(|_| at.http.is_redirect()) {
        redirects += 1;
        if !seen.insert(next) {
            looped = true;
            hops.push(format!("loop to {next}"));
            break;
        }
        let Some(&url) = by_url.get(next) else {
            hops.push(format!("not crawled {next}"));
            break;
        };
        walked.insert(url.http.url.as_str());
        hops.push(format!("{} {}", url.http.status, url.http.url));
        at = url;
        if hops.len() > 20 {
            break;
        }
    }
    vec![
        start.http.url.as_str().into(),
        redirects.into(),
        at.http.url.as_str().into(),
        Cell::Int(at.http.status.into()),
        looped.into(),
        hops.join(" → ").into(),
    ]
}

/// Raw HTML against the rendered DOM for every rendered page.
fn javascript(store: &Store, urls: &[StoredUrl]) -> Result<Table> {
    let headers = vec![
        "Address", "Render Error", "Settled", "Render Time (ms)", "Console Errors", "Failed Requests",
        "Title Raw", "Title Rendered", "H1 Raw", "H1 Rendered", "Canonical Raw", "Canonical Rendered",
        "Meta Robots Raw", "Meta Robots Rendered", "Word Count Raw", "Word Count Rendered",
        "Links Raw", "Links Rendered", "Links Only In Rendered",
    ];
    let mut select = store.connection().prepare(
        "SELECT error, settled, total_ms, console_errors, resources FROM renders WHERE url_id = ?1",
    )?;
    let mut rows = Vec::new();
    for url in urls.iter().filter(|url| url.raw.is_some()) {
        let render = select
            .query_row(params![url.id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .ok();
        let Some((error, settled, total_ms, console, resources)) = render else { continue };
        let console = serde_json::from_str::<Vec<String>>(&console).map_or(0, |errors| errors.len());
        let failed = serde_json::from_str::<Vec<serde_json::Value>>(&resources).map_or(0, |resources| {
            resources
                .iter()
                .filter(|resource| resource["status"].as_u64().is_none_or(|status| status == 0 || status >= 400))
                .count()
        });
        let raw = url.raw.as_ref().context("filtered on raw")?;
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
        rows.push(vec![
            url.http.url.as_str().into(),
            error.into(),
            settled.into(),
            total_ms.into(),
            console.into(),
            failed.into(),
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
        ]);
    }
    Ok(Table { name: "javascript", headers, rows })
}

fn links(store: &Store, crawl_id: i64) -> Result<Table> {
    let headers = vec!["From", "To", "To Status Code", "Anchor", "Rel", "Nofollow", "Position", "Image", "Document"];
    let mut select = store.connection().prepare(
        "SELECT u.url, l.to_url, t.status, l.anchor, l.rel, l.nofollow, l.position, l.image, l.source
         FROM links l
         JOIN urls u ON u.id = l.from_url_id
         LEFT JOIN urls t ON t.crawl_id = l.crawl_id AND t.url = l.to_url
         WHERE l.crawl_id = ?1 AND l.source = (
             CASE WHEN EXISTS (SELECT 1 FROM facts f WHERE f.url_id = l.from_url_id AND f.source = 'rendered')
             THEN 'rendered' ELSE 'raw' END)
         ORDER BY l.rowid
         LIMIT ?2",
    )?;
    let rows = select
        .query_map(params![crawl_id, MAX_ROWS as i64], |row| {
            Ok(vec![
                Cell::Text(row.get(0)?),
                Cell::Text(row.get(1)?),
                row.get::<_, Option<i64>>(2)?.map_or(Cell::Empty, Cell::Int),
                Cell::Text(row.get(3)?),
                Cell::Text(row.get(4)?),
                Cell::Bool(row.get(5)?),
                Cell::Text(row.get(6)?),
                Cell::Bool(row.get(7)?),
                Cell::Text(row.get(8)?),
            ])
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Table { name: "links", headers, rows })
}

fn issues(store: &Store, crawl_id: i64) -> Result<Table> {
    let headers = vec!["Address", "Severity", "Category", "Issue", "Code", "Detail"];
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
    let rows = rows
        .into_iter()
        .map(|(url, code, detail)| {
            let check = seo_checks::find(&code);
            vec![
                url.into(),
                check.map(|check| check.severity.as_str()).into(),
                check.map(|check| check.category).into(),
                check.map(|check| check.title).into(),
                code.into(),
                detail.into(),
            ]
        })
        .collect();
    Ok(Table { name: "issues", headers, rows })
}

/// How many pages each check flagged, most severe first.
fn summary(store: &Store, crawl_id: i64) -> Result<Table> {
    let headers = vec!["Severity", "Category", "Issue", "Code", "URLs", "Description"];
    let mut select = store
        .connection()
        .prepare("SELECT code, count(DISTINCT url_id) FROM issues WHERE crawl_id = ?1 GROUP BY code")?;
    let counts: HashMap<String, i64> = select
        .query_map(params![crawl_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut checks: Vec<_> = seo_checks::CATALOG.iter().filter(|check| counts.contains_key(check.code)).collect();
    checks.sort_by_key(|check| (check.severity, check.category, std::cmp::Reverse(counts[check.code])));
    let rows = checks
        .into_iter()
        .map(|check| {
            vec![
                check.severity.as_str().into(),
                check.category.into(),
                check.title.into(),
                check.code.into(),
                counts[check.code].into(),
                check.description.into(),
            ]
        })
        .collect();
    Ok(Table { name: "summary", headers, rows })
}

pub fn write_csv(tables: &[Table], dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    for table in tables {
        let mut writer = csv::Writer::from_path(dir.join(format!("{}.csv", table.name)))?;
        writer.write_record(&table.headers)?;
        for row in &table.rows {
            writer.write_record(row.iter().map(|cell| match cell {
                Cell::Text(text) => text.clone(),
                Cell::Int(value) => value.to_string(),
                Cell::Bool(value) => value.to_string(),
                Cell::Empty => String::new(),
            }))?;
        }
        writer.flush()?;
    }
    Ok(())
}

pub fn write_xlsx(tables: &[Table], path: &Path) -> Result<()> {
    use rust_xlsxwriter::{Format, Workbook};
    /// Longest text a cell holds.
    const MAX_CELL: usize = 32_767;
    let mut workbook = Workbook::new();
    let bold = Format::new().set_bold();
    for table in tables {
        let sheet = workbook.add_worksheet();
        sheet.set_name(table.name)?;
        for (col, header) in table.headers.iter().enumerate() {
            sheet.write_string_with_format(0, col as u16, *header, &bold)?;
        }
        for (i, row) in table.rows.iter().take(MAX_ROWS).enumerate() {
            let r = i as u32 + 1;
            for (col, cell) in row.iter().enumerate() {
                let col = col as u16;
                match cell {
                    Cell::Text(text) if text.len() > MAX_CELL => {
                        let cut: String = text.chars().take(MAX_CELL / 4).collect();
                        sheet.write_string(r, col, cut)?;
                    }
                    Cell::Text(text) => {
                        sheet.write_string(r, col, text)?;
                    }
                    Cell::Int(value) => {
                        sheet.write_number(r, col, *value as f64)?;
                    }
                    Cell::Bool(value) => {
                        sheet.write_boolean(r, col, *value)?;
                    }
                    Cell::Empty => {}
                }
            }
        }
        sheet.set_freeze_panes(1, 0)?;
        let last_row = table.rows.len().min(MAX_ROWS) as u32;
        if !table.headers.is_empty() {
            sheet.autofilter(0, 0, last_row, table.headers.len() as u16 - 1)?;
        }
    }
    workbook.save(path)?;
    Ok(())
}
