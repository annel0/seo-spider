//! Storage for crawls: a SQLite file by default, or a PostgreSQL or MySQL
//! database. Each URL row carries its HTTP response; raw and rendered facts
//! are kept as compressed JSON next to the columns exports and queries need.
//! The logic and its SQL are written once, over `Backend`; each database
//! is a backend (see `sql`).

use std::cell::RefCell;
use std::ffi::OsStr;
use std::path::Path;

mod graph;
mod mysql;
mod postgres;
mod schema;
mod sql;
mod sqlite;
mod view;

pub use graph::{LinkRow, Source, LINK, REDIRECT};
pub use sql::{Backend, Dialect, FromValue, Param, Row, Value};
pub use view::{CrawlInfo, Overview, RowQuery, RowsPage, StoredRow, FILTERS, SCOPES};

use anyhow::{bail, Context, Result};
use schema::{LazyIndex, BODIES, FACTS, ISSUES, LINKS, LINKS_FROM, LINKS_TO, RENDERS, STATES, URLS};
use seo_page::{HttpInfo, PageFacts, RenderSummary};

/// zstd frames start with these bytes; JSON never does.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// `PageFacts` from a stored `facts.data` value, compressed or not.
pub fn decode_facts(data: &[u8]) -> Result<PageFacts> {
    if data.starts_with(&ZSTD_MAGIC) {
        let json = zstd::stream::decode_all(data)?;
        Ok(serde_json::from_slice(&json)?)
    } else {
        Ok(serde_json::from_slice(data)?)
    }
}

/// `facts` as stored: JSON compressed at `level`, or plain JSON at level 0.
pub fn encode_facts(facts: &PageFacts, level: i32) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(facts)?;
    if level > 0 {
        Ok(zstd::bulk::compress(&json, level)?)
    } else {
        Ok(json)
    }
}

/// A crawl store: one connection to a crawl file or database.
///
/// Calls take `&self`; the connection is used by one call at a time, so a
/// callback of `query` or `for_each_url_chunk` must not call the store.
pub struct Store {
    db: RefCell<Box<dyn Backend>>,
}

/// Whether a `--db` value names a database server rather than a file.
pub fn is_server(target: &OsStr) -> bool {
    let text = target.to_string_lossy();
    ["postgres://", "postgresql://", "mysql://"].iter().any(|scheme| text.starts_with(scheme))
}

/// A `--db` value fit to print: a server address without its password.
pub fn describe(target: &OsStr) -> String {
    let text = target.to_string_lossy();
    if !is_server(target) {
        return text.into_owned();
    }
    match url::Url::parse(&text) {
        Ok(mut url) => {
            if url.password().is_some() {
                let _ = url.set_password(Some("***"));
            }
            url.to_string()
        }
        Err(_) => "(database address)".into(),
    }
}

/// Now in UTC as SQLite's `datetime('now')` writes it.
fn now() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

impl Store {
    /// Opens a crawl file (a path) or database (`postgres://user@host/db`,
    /// `mysql://user@host/db`; the password from the address or from
    /// `SEO_SPIDER_DB_PASSWORD`, `PGPASSWORD`, `MYSQL_PWD`), and creates the
    /// schema where it is missing.
    pub fn open(target: impl AsRef<OsStr>) -> Result<Self> {
        let store = Store::connect(target.as_ref(), false)?;
        store.create_schema()?;
        Ok(store)
    }

    /// Opens a crawl file or database for reading, alongside a crawl writing
    /// to it; a file is opened read-only.
    pub fn open_read(target: impl AsRef<OsStr>) -> Result<Self> {
        Store::connect(target.as_ref(), true)
    }

    fn connect(target: &OsStr, read_only: bool) -> Result<Self> {
        let text = target.to_string_lossy();
        let backend: Box<dyn Backend> = if text.starts_with("mysql://") {
            Box::new(mysql::Mysql::connect(&text)?)
        } else if is_server(target) {
            Box::new(postgres::Postgres::connect(&text)?)
        } else if read_only {
            Box::new(sqlite::Sqlite::open_read(Path::new(target))?)
        } else {
            Box::new(sqlite::Sqlite::open(Path::new(target))?)
        };
        Ok(Store { db: RefCell::new(backend) })
    }

    fn create_schema(&self) -> Result<()> {
        let mut db = self.db.borrow_mut();
        let dialect = db.dialect();
        db.batch(schema::schema(dialect)).context("create schema")?;
        if dialect == Dialect::Sqlite {
            // Files from before `crawls.stopped`.
            let mut has = false;
            db.query("SELECT count(*) FROM pragma_table_info('crawls') WHERE name = 'stopped'", &[], &mut |row| {
                has = row.get::<i64>(0)? > 0;
                Ok(())
            })?;
            if !has {
                db.batch("ALTER TABLE crawls ADD COLUMN stopped INTEGER NOT NULL DEFAULT 0;")?;
            }
        }
        Ok(())
    }

    pub fn dialect(&self) -> Dialect {
        self.db.borrow().dialect()
    }

    /// The rows of a query, one at a time. SQL as `sql` describes it:
    /// standard, with `?1`-style parameters.
    pub fn query(&self, sql: &str, params: &[Param], mut each: impl FnMut(Row) -> Result<()>) -> Result<()> {
        self.db.borrow_mut().query(sql, params, &mut each).with_context(|| format!("query {}", first_line(sql)))
    }

    /// Every row of a query.
    pub fn query_rows(&self, sql: &str, params: &[Param]) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        self.query(sql, params, |row| {
            rows.push(row);
            Ok(())
        })?;
        Ok(rows)
    }

    /// The first row of a query, if any.
    pub fn query_row(&self, sql: &str, params: &[Param]) -> Result<Option<Row>> {
        let mut first = None;
        self.query(sql, params, |row| {
            first.get_or_insert(row);
            Ok(())
        })?;
        Ok(first)
    }

    pub fn execute(&self, sql: &str, params: &[Param]) -> Result<u64> {
        self.db.borrow_mut().execute(sql, params).with_context(|| format!("run {}", first_line(sql)))
    }

    /// `work` in one transaction: committed if it succeeds, rolled back if not.
    fn transaction<T>(&self, work: impl FnOnce(&mut dyn Backend) -> Result<T>) -> Result<T> {
        let mut db = self.db.borrow_mut();
        db.begin()?;
        match work(db.as_mut()) {
            Ok(value) => {
                db.commit()?;
                Ok(value)
            }
            Err(error) => {
                let _ = db.rollback();
                Err(error)
            }
        }
    }

    pub fn start_crawl(&self, start_url: &str, config_json: &str) -> Result<i64> {
        self.db.borrow_mut().insert_returning_id(
            "INSERT INTO crawls (start_url, started_at, config) VALUES (?1, ?2, ?3)",
            &params![start_url, now(), config_json],
        )
    }

    /// Closes a crawl; `stopped` when it was asked to stop with URLs still
    /// queued.
    pub fn finish_crawl(&self, crawl_id: i64, stopped: bool) -> Result<()> {
        self.execute("UPDATE crawls SET finished_at = ?2, stopped = ?3 WHERE id = ?1", &params![crawl_id, now(), stopped])?;
        Ok(())
    }

    /// Opens a closed or interrupted crawl again, to go on with it under
    /// `config_json` (its settings, perhaps with a higher limit).
    pub fn reopen_crawl(&self, crawl_id: i64, config_json: &str) -> Result<()> {
        let exists = self.query_row("SELECT id FROM crawls WHERE id = ?1", &params![crawl_id])?.is_some();
        anyhow::ensure!(exists, "no crawl {crawl_id}");
        self.execute(
            "UPDATE crawls SET finished_at = NULL, stopped = 0, config = ?2 WHERE id = ?1",
            &params![crawl_id, config_json],
        )?;
        Ok(())
    }

    /// Replaces the settings a crawl runs with (after the auto mode decided).
    pub fn update_crawl_config(&self, crawl_id: i64, config_json: &str) -> Result<()> {
        self.execute("UPDATE crawls SET config = ?2 WHERE id = ?1", &params![crawl_id, config_json])?;
        Ok(())
    }

    /// The URLs a crawl has written, in id order (which is the order it
    /// found them), without their facts.
    pub fn saved_urls(&self, crawl_id: i64) -> Result<Vec<SavedUrl>> {
        let mut urls = Vec::new();
        self.query(
            "SELECT id, url, internal, depth, status, content_type, error, blocked_by_robots
             FROM urls WHERE crawl_id = ?1 ORDER BY id",
            &params![crawl_id],
            |row| {
                urls.push(SavedUrl {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    internal: row.get(2)?,
                    depth: row.get(3)?,
                    status: row.get(4)?,
                    content_type: row.get(5)?,
                    error: row.get(6)?,
                    blocked_by_robots: row.get(7)?,
                });
                Ok(())
            },
        )?;
        Ok(urls)
    }

    /// Removes URLs and all that hangs on them, for a continued crawl to
    /// write them again.
    pub fn delete_urls(&self, ids: &[i64]) -> Result<()> {
        for chunk in ids.chunks(500) {
            let list = chunk.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            self.transaction(|db| {
                for (table, column) in
                    [("facts", "url_id"), ("renders", "url_id"), ("bodies", "url_id"), ("issues", "url_id"), ("links", "from_url_id"), ("urls", "id")]
                {
                    db.execute(&format!("DELETE FROM {table} WHERE {column} IN ({list})"), &[])?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Builds an index unless it is there.
    fn ensure_index(&self, index: &LazyIndex) -> Result<()> {
        let mut db = self.db.borrow_mut();
        if db.index_exists(index.table, index.name)? {
            return Ok(());
        }
        let (name, table) = (index.name, index.table);
        let sql = match db.dialect() {
            Dialect::Sqlite => format!("CREATE INDEX IF NOT EXISTS {name} ON {table} ({})", index.sqlite),
            Dialect::Postgres => format!("CREATE INDEX IF NOT EXISTS {name} ON {table} ({})", index.postgres),
            Dialect::Mysql => format!("CREATE INDEX {name} ON {table} ({})", index.mysql),
        };
        db.batch(&sql).with_context(|| format!("create index {name}"))
    }

    /// Builds the indexes for per-URL link lookups, if missing.
    pub fn ensure_read_indexes(&self) -> Result<()> {
        self.ensure_index(&LINKS_TO)?;
        self.ensure_index(&LINKS_FROM)
    }

    /// The settings a crawl ran with, as JSON.
    pub fn crawl_config(&self, crawl_id: i64) -> Result<String> {
        match self.query_row("SELECT config FROM crawls WHERE id = ?1", &params![crawl_id])? {
            Some(row) => row.get(0),
            None => bail!("no crawl {crawl_id}"),
        }
    }

    /// The most recent crawl.
    pub fn last_crawl(&self) -> Result<Option<i64>> {
        match self.query_row("SELECT max(id) FROM crawls", &[])? {
            Some(row) => row.get(0),
            None => Ok(None),
        }
    }

    /// Turns off foreign key checks for this connection (SQLite: the other
    /// databases have none): the crawl's writer only inserts rows whose
    /// references it made itself, and the lookups cost a third of a link
    /// row's insert.
    pub fn bulk_writes(&self) -> Result<()> {
        if self.dialect() == Dialect::Sqlite {
            self.db.borrow_mut().batch("PRAGMA foreign_keys = OFF;")?;
        }
        Ok(())
    }

    /// The first URL id no row uses yet; a crawl numbers its URLs from it.
    pub fn next_url_id(&self) -> Result<i64> {
        match self.query_row("SELECT coalesce(max(id), 0) + 1 FROM urls", &[])? {
            Some(row) => row.get(0),
            None => Ok(1),
        }
    }

    /// Applies `writes` in one transaction, each table's rows in one bulk
    /// insert.
    pub fn apply(&mut self, crawl_id: i64, writes: &[Write]) -> Result<()> {
        let (mut urls, mut facts, mut links, mut renders, mut bodies, mut issues) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for write in writes {
            match write {
                Write::Response { id, http, internal, depth } => {
                    urls.push(
                        params![
                            *id,
                            crawl_id,
                            &http.url,
                            http.status,
                            http.location.as_ref(),
                            http.content_type.as_ref(),
                            http.ttfb_ms,
                            http.size,
                            http.error.as_ref(),
                            serde_json::to_string(&http.headers)?,
                            *internal,
                            *depth,
                        ]
                        .to_vec(),
                    );
                }
                Write::Facts { url_id, facts: page, data, links: with_links } => {
                    let source = page.source.as_str();
                    facts.push(
                        params![
                            *url_id,
                            source,
                            page.title(),
                            page.description(),
                            page.h1().next(),
                            page.canonicals.first(),
                            (!page.meta_robots.is_empty()).then(|| page.meta_robots.join(", ")),
                            page.word_count,
                            format!("{:016x}", page.text_hash),
                            page.links.len(),
                            data,
                        ]
                        .to_vec(),
                    );
                    if *with_links {
                        for link in &page.links {
                            links.push(
                                params![
                                    crawl_id,
                                    *url_id,
                                    source,
                                    &link.href,
                                    &link.anchor,
                                    link.rel.join(" "),
                                    link.nofollow,
                                    link.position.as_str(),
                                    link.image,
                                ]
                                .to_vec(),
                            );
                        }
                    }
                }
                Write::Render { url_id, render } => {
                    renders.push(
                        params![
                            *url_id,
                            &render.final_url,
                            render.error.as_ref(),
                            render.dcl_ms,
                            render.total_ms,
                            render.settled,
                            render.load_fired,
                            &render.console_errors_json,
                            &render.resources_json,
                        ]
                        .to_vec(),
                    );
                }
                Write::Body { url_id, kind, encoding, size, data } => {
                    bodies.push(params![*url_id, kind.as_str(), *encoding, *size, data].to_vec());
                }
                Write::Issues { url_id, issues: found } => {
                    for (code, detail) in found {
                        issues.push(params![crawl_id, *url_id, *code, detail.as_ref()].to_vec());
                    }
                }
            }
        }
        self.transaction(|db| {
            for (columns, rows) in
                [(&URLS, &urls), (&FACTS, &facts), (&LINKS, &links), (&RENDERS, &renders), (&BODIES, &bodies), (&ISSUES, &issues)]
            {
                if !rows.is_empty() {
                    db.insert_rows(columns, rows)?;
                }
            }
            Ok(())
        })
    }

    /// A stored document as written, with its encoding.
    pub fn body(&self, url_id: i64, kind: BodyKind) -> Result<Option<(String, Vec<u8>)>> {
        match self.query_row("SELECT encoding, data FROM bodies WHERE url_id = ?1 AND kind = ?2", &params![url_id, kind.as_str()])? {
            Some(mut row) => Ok(Some((row.get(0)?, row.take_blob(1)?.unwrap_or_default()))),
            None => Ok(None),
        }
    }

    /// Sets (url id, click depth, listed in a sitemap, blocked by robots.txt)
    /// for each URL.
    pub fn set_crawl_states(&mut self, states: &[(i64, Option<u32>, bool, bool)]) -> Result<()> {
        let rows: Vec<Vec<Param>> = states
            .iter()
            .map(|(url_id, depth, in_sitemap, blocked)| params![*url_id, *depth, *in_sitemap, *blocked].to_vec())
            .collect();
        self.transaction(|db| db.update_rows(&STATES, &rows))
    }

    /// Replaces the crawl's issues.
    pub fn save_issues(&mut self, crawl_id: i64, issues: &[(i64, &str, Option<String>)]) -> Result<()> {
        let rows: Vec<Vec<Param>> =
            issues.iter().map(|(url_id, code, detail)| params![crawl_id, *url_id, *code, detail.as_ref()].to_vec()).collect();
        self.transaction(|db| {
            db.execute("DELETE FROM issues WHERE crawl_id = ?1", &params![crawl_id])?;
            db.insert_rows(&ISSUES, &rows)
        })
    }

    /// Every URL of a crawl with its facts, in insertion order.
    pub fn urls(&self, crawl_id: i64) -> Result<Vec<StoredUrl>> {
        let mut urls = Vec::new();
        self.for_each_url_chunk(crawl_id, 4096, |chunk| {
            for encoded in chunk {
                urls.push(encoded.decode()?);
            }
            Ok(())
        })?;
        Ok(urls)
    }

    /// Every URL of a crawl in id order, `chunk` at a time, with its facts
    /// still encoded: a reader decodes them where it likes (in parallel) and
    /// never holds more than a chunk of them.
    pub fn for_each_url_chunk(
        &self,
        crawl_id: i64,
        chunk: usize,
        mut each: impl FnMut(Vec<EncodedUrl>) -> Result<()>,
    ) -> Result<()> {
        let mut batch = Vec::with_capacity(chunk);
        self.query(&format!("{SELECT_URL} WHERE u.crawl_id = ?1 ORDER BY u.id"), &params![crawl_id], |row| {
            batch.push(encoded_url(row)?);
            if batch.len() >= chunk {
                each(std::mem::replace(&mut batch, Vec::with_capacity(chunk)))?;
            }
            Ok(())
        })?;
        if !batch.is_empty() {
            each(batch)?;
        }
        Ok(())
    }

    /// One URL with its facts decoded.
    pub fn url(&self, id: i64) -> Result<Option<StoredUrl>> {
        match self.query_row(&format!("{SELECT_URL} WHERE u.id = ?1"), &params![id])? {
            Some(row) => Ok(Some(encoded_url(row)?.decode()?)),
            None => Ok(None),
        }
    }
}

fn first_line(sql: &str) -> &str {
    sql.trim().lines().next().unwrap_or("")
}

/// A URL with its documents' facts and render, as `encoded_url` reads it.
pub(crate) const SELECT_URL: &str = "SELECT u.id, u.url, u.status, u.location, u.content_type, u.ttfb_ms, u.size, u.error, u.headers,
        u.depth, u.in_sitemap, u.blocked_by_robots, u.internal, fw.data, fr.data,
        r.url_id, r.error, r.settled, r.console_errors, r.resources
    FROM urls u
    LEFT JOIN facts fw ON fw.url_id = u.id AND fw.source = 'raw'
    LEFT JOIN facts fr ON fr.url_id = u.id AND fr.source = 'rendered'
    LEFT JOIN renders r ON r.url_id = u.id";

/// Text columns are taken out of the row, not copied: the checks and
/// exports read every URL of a crawl through here.
pub(crate) fn encoded_url(mut row: Row) -> Result<EncodedUrl> {
    let headers = row.take_string(8)?;
    Ok(EncodedUrl {
        url: StoredUrl {
            id: row.get(0)?,
            http: HttpInfo {
                url: row.take_string(1)?,
                status: row.get(2)?,
                location: row.take_text(3)?,
                content_type: row.take_text(4)?,
                ttfb_ms: row.get(5)?,
                size: row.get(6)?,
                error: row.take_text(7)?,
                headers: serde_json::from_str(&headers).unwrap_or_default(),
            },
            internal: row.get(12)?,
            depth: row.get(9)?,
            in_sitemap: row.get(10)?,
            blocked_by_robots: row.get(11)?,
            raw: None,
            rendered: None,
            render: None,
        },
        raw: row.take_blob(13)?,
        rendered: row.take_blob(14)?,
        render: match row.get::<Option<i64>>(15)? {
            Some(_) => Some((row.take_text(16)?, row.get(17)?, row.take_string(18)?, row.take_string(19)?)),
            None => None,
        },
    })
}

/// One URL of a crawl with everything the cross-page checks and exports read.
#[derive(Debug, Clone)]
pub struct StoredUrl {
    pub id: i64,
    pub http: HttpInfo,
    pub internal: bool,
    pub depth: Option<u32>,
    pub in_sitemap: bool,
    pub blocked_by_robots: bool,
    pub raw: Option<PageFacts>,
    pub rendered: Option<PageFacts>,
    pub render: Option<RenderSummary>,
}

impl StoredUrl {
    /// Rendered facts when the page was rendered, raw ones otherwise.
    pub fn facts(&self) -> Option<&PageFacts> {
        self.rendered.as_ref().or(self.raw.as_ref())
    }
}

/// A URL as `saved_urls` lists it: enough to know it, not its facts.
#[derive(Debug, Clone)]
pub struct SavedUrl {
    pub id: i64,
    pub url: String,
    pub internal: bool,
    pub depth: Option<u32>,
    pub status: u16,
    pub content_type: Option<String>,
    pub error: Option<String>,
    pub blocked_by_robots: bool,
}

/// A URL whose facts are still as stored; `decode` reads them.
pub struct EncodedUrl {
    pub url: StoredUrl,
    pub raw: Option<Vec<u8>>,
    pub rendered: Option<Vec<u8>>,
    /// (error, settled, console errors JSON, resources JSON) of the render.
    pub render: Option<(Option<String>, bool, String, String)>,
}

impl EncodedUrl {
    pub fn decode(self) -> Result<StoredUrl> {
        let EncodedUrl { mut url, raw, rendered, render } = self;
        url.raw = raw.as_deref().map(decode_facts).transpose()?;
        url.rendered = rendered.as_deref().map(decode_facts).transpose()?;
        url.render = render.map(|(error, settled, console, resources)| render_summary(error, settled, &console, &resources));
        Ok(url)
    }
}

/// A stored render record as the checks read it. A request failed when it
/// got an error status, or no response for a reason other than a block (a
/// tracker the crawl chose not to load is not the page's failure).
pub fn render_summary(error: Option<String>, settled: bool, console: &str, resources: &str) -> RenderSummary {
    let console: Vec<String> = serde_json::from_str(console).unwrap_or_default();
    let resources: Vec<serde_json::Value> = serde_json::from_str(resources).unwrap_or_default();
    let failed: Vec<&serde_json::Value> = resources
        .iter()
        .filter(|resource| {
            let status = resource["status"].as_u64().unwrap_or(0);
            let error = resource["error"].as_str();
            status >= 400 || (status == 0 && error.is_some_and(|error| !error.to_ascii_lowercase().contains("block")))
        })
        .collect();
    RenderSummary {
        error,
        settled,
        console_errors: console.len(),
        first_console_error: console.first().map(|error| error.lines().next().unwrap_or("").chars().take(300).collect()),
        failed_requests: failed.len(),
        first_failed_request: failed.first().map(|resource| {
            format!(
                "{} {}",
                resource["status"].as_u64().unwrap_or(0),
                resource["url"].as_str().unwrap_or("").chars().take(300).collect::<String>()
            )
        }),
    }
}

/// A render outcome as stored.
#[derive(Debug, Clone)]
pub struct RenderRecord {
    pub final_url: String,
    pub error: Option<String>,
    pub dcl_ms: u64,
    pub total_ms: u64,
    pub settled: bool,
    pub load_fired: bool,
    /// JSON array of console error messages.
    pub console_errors_json: String,
    /// JSON array of the render's requests.
    pub resources_json: String,
}

/// Which document a stored body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Raw,
    Rendered,
}

impl BodyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BodyKind::Raw => "raw",
            BodyKind::Rendered => "rendered",
        }
    }
}

/// One write of a crawl. The crawler prepares everything that costs CPU
/// (JSON, compression) on its worker threads, so applying a batch is only
/// inserts.
#[derive(Debug)]
pub enum Write {
    /// A response. The crawl assigns ids itself (see `next_url_id`) so it
    /// never waits for the database to learn one.
    /// `depth` is the clicks from a start URL as the crawl found it; the
    /// crawl's end (`set_crawl_states`) settles it.
    Response { id: i64, http: HttpInfo, internal: bool, depth: Option<u32> },
    /// Facts of one document of a URL, with `PageFacts` already encoded by
    /// `encode_facts`; `links` also stores its outgoing links as rows.
    Facts { url_id: i64, facts: Box<PageFacts>, data: Vec<u8>, links: bool },
    Render { url_id: i64, render: Box<RenderRecord> },
    /// A whole document; `encoding` is `identity` or `zstd`.
    Body { url_id: i64, kind: BodyKind, encoding: &'static str, size: usize, data: Vec<u8> },
    /// A page's issues found while the crawl runs, for a live view; the
    /// checks after the crawl replace them all.
    Issues { url_id: i64, issues: Vec<(&'static str, Option<String>)> },
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use seo_page::Source;

    /// Database servers to run the store's tests on besides SQLite, from
    /// `SEO_SPIDER_TEST_POSTGRES` and `SEO_SPIDER_TEST_MYSQL` (addresses of
    /// databases the tests may empty).
    pub(crate) fn servers() -> Vec<String> {
        ["SEO_SPIDER_TEST_POSTGRES", "SEO_SPIDER_TEST_MYSQL"].iter().filter_map(|name| std::env::var(name).ok()).collect()
    }

    /// The servers' tests share their databases: one at a time.
    pub(crate) static SERVER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An empty store at `target`: a fresh file, or a server database with
    /// the tables dropped.
    pub(crate) fn empty(target: &str) -> Store {
        if is_server(OsStr::new(target)) {
            let store = Store::open_read(target).unwrap();
            store.execute("DROP TABLE IF EXISTS crawls, urls, facts, links, renders, bodies, issues, inlinks", &[]).unwrap();
        } else {
            let _ = std::fs::remove_file(target);
        }
        Store::open(target).unwrap()
    }

    fn round_trip(target: &str) {
        let mut store = empty(target);
        let crawl = store.start_crawl("https://example.com/", "{}").unwrap();
        let http = HttpInfo {
            url: "https://example.com/".into(),
            status: 200,
            content_type: Some("text/html".into()),
            headers: vec![("x-robots-tag".into(), "noindex".into())],
            ..Default::default()
        };
        let id = store.next_url_id().unwrap();
        let mut facts = seo_page::extract(
            "<title>T</title><a href='/b'>b</a>",
            &url::Url::parse("https://example.com/").unwrap(),
            Source::Raw,
        );
        // A NUL in a title: PostgreSQL text cannot hold it.
        facts.titles = vec!["T\u{0} ёж".into()];
        let data = encode_facts(&facts, 3).unwrap();
        assert!(data.starts_with(&ZSTD_MAGIC));
        assert_eq!(decode_facts(&serde_json::to_vec(&facts).unwrap()).unwrap().title(), Some("T\u{0} ёж"));
        store
            .apply(
                crawl,
                &[
                    Write::Response { id, http, internal: true, depth: Some(0) },
                    Write::Facts { url_id: id, facts: Box::new(facts), data, links: true },
                    Write::Body { url_id: id, kind: BodyKind::Raw, encoding: "identity", size: 2, data: b"ok".to_vec() },
                    Write::Issues { url_id: id, issues: vec![("title_missing", Some("x".into()))] },
                ],
            )
            .unwrap();
        assert_eq!(store.next_url_id().unwrap(), id + 1);
        assert_eq!(store.body(id, BodyKind::Raw).unwrap(), Some(("identity".into(), b"ok".to_vec())));
        store.set_crawl_states(&[(id, Some(0), true, false)]).unwrap();
        store.save_issues(crawl, &[(id, "title_too_short", None)]).unwrap();
        let urls = store.urls(crawl).unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].http.header("X-Robots-Tag"), Some("noindex"));
        assert_eq!(urls[0].raw.as_ref().unwrap().title(), Some("T\u{0} ёж"));
        let title: String = store.query_row("SELECT title FROM facts", &[]).unwrap().unwrap().get(0).unwrap();
        assert!(title.ends_with(" ёж"), "{title:?}");
        assert_eq!((urls[0].depth, urls[0].in_sitemap), (Some(0), true));
        let links: i64 = store.query_row("SELECT count(*) FROM links", &[]).unwrap().unwrap().get(0).unwrap();
        assert_eq!(links, 1);
        let issues: Vec<String> =
            store.query_rows("SELECT code FROM issues", &[]).unwrap().iter().map(|row| row.get(0).unwrap()).collect();
        assert_eq!(issues, ["title_too_short"]);
        store.finish_crawl(crawl, true).unwrap();
        assert!(store.crawls().unwrap()[0].stopped);
        store.reopen_crawl(crawl, "{\"a\":1}").unwrap();
        assert_eq!(store.crawl_config(crawl).unwrap(), "{\"a\":1}");
        assert!(store.crawls().unwrap()[0].finished_at.is_none());
        assert_eq!(store.saved_urls(crawl).unwrap()[0].url, "https://example.com/");
        store.ensure_read_indexes().unwrap();
        store.ensure_read_indexes().unwrap();
        store.delete_urls(&[id]).unwrap();
        assert!(store.url(id).unwrap().is_none());
        assert_eq!(store.last_crawl().unwrap(), Some(crawl));
    }

    #[test]
    fn round_trips_a_page() {
        let path = std::env::temp_dir().join(format!("seo-store-{}.sqlite", std::process::id()));
        round_trip(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn round_trips_a_page_on_servers() {
        let _one = SERVER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for target in servers() {
            round_trip(&target);
        }
    }

    #[test]
    fn dates_are_civil() {
        let date = now();
        assert_eq!(date.len(), 19);
        assert!(date.starts_with("20"));
    }

    #[test]
    fn hides_passwords() {
        assert_eq!(describe(OsStr::new("postgres://seo:secret@db:5432/seo")), "postgres://seo:***@db:5432/seo");
        assert_eq!(describe(OsStr::new("crawl.sqlite")), "crawl.sqlite");
    }
}
