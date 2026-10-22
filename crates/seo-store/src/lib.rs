//! SQLite storage for crawls. One file holds any number of crawls; each URL
//! row carries its HTTP response, and raw and rendered facts are kept as JSON
//! next to the columns exports and queries need.

use std::path::Path;

mod view;

pub use view::{CrawlInfo, Overview, RowQuery, RowsPage, StoredRow, FILTERS, SCOPES};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use seo_page::{HttpInfo, PageFacts, RenderSummary};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA temp_store = MEMORY;
PRAGMA cache_size = -65536;

CREATE TABLE IF NOT EXISTS crawls (
    id INTEGER PRIMARY KEY,
    start_url TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT,
    config TEXT NOT NULL DEFAULT '{}',
    -- Asked to stop before its queue ran out: `continue` can take it on.
    stopped INTEGER NOT NULL DEFAULT 0
);

-- Every URL the crawl requested, redirect hops included.
CREATE TABLE IF NOT EXISTS urls (
    id INTEGER PRIMARY KEY,
    crawl_id INTEGER NOT NULL REFERENCES crawls(id),
    url TEXT NOT NULL,
    status INTEGER NOT NULL,
    location TEXT,
    content_type TEXT,
    ttfb_ms INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    headers TEXT NOT NULL DEFAULT '[]',
    -- Same site as the start URL (subdomains too when the crawl covers them).
    internal INTEGER NOT NULL,
    depth INTEGER,
    in_sitemap INTEGER NOT NULL DEFAULT 0,
    blocked_by_robots INTEGER NOT NULL DEFAULT 0,
    UNIQUE (crawl_id, url)
);

-- Facts read from one document of a URL: the raw body or the rendered DOM.
CREATE TABLE IF NOT EXISTS facts (
    url_id INTEGER NOT NULL REFERENCES urls(id),
    source TEXT NOT NULL,
    title TEXT,
    description TEXT,
    h1 TEXT,
    canonical TEXT,
    robots TEXT,
    word_count INTEGER NOT NULL,
    text_hash TEXT NOT NULL,
    links INTEGER NOT NULL,
    -- PageFacts as JSON, or zstd-compressed JSON (see `decode_facts`).
    data BLOB NOT NULL,
    PRIMARY KEY (url_id, source)
);

CREATE TABLE IF NOT EXISTS links (
    crawl_id INTEGER NOT NULL REFERENCES crawls(id),
    from_url_id INTEGER NOT NULL REFERENCES urls(id),
    source TEXT NOT NULL,
    to_url TEXT NOT NULL,
    anchor TEXT NOT NULL,
    rel TEXT NOT NULL,
    nofollow INTEGER NOT NULL,
    position TEXT NOT NULL,
    image INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS renders (
    url_id INTEGER PRIMARY KEY REFERENCES urls(id),
    final_url TEXT NOT NULL,
    error TEXT,
    dcl_ms INTEGER NOT NULL,
    total_ms INTEGER NOT NULL,
    settled INTEGER NOT NULL,
    load_fired INTEGER NOT NULL,
    console_errors TEXT NOT NULL,
    resources TEXT NOT NULL
);

-- Documents kept whole when the crawl dumps pages: the raw response body and
-- the rendered DOM, compressed.
CREATE TABLE IF NOT EXISTS bodies (
    url_id INTEGER NOT NULL REFERENCES urls(id),
    kind TEXT NOT NULL,
    encoding TEXT NOT NULL,
    size INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY (url_id, kind)
);

CREATE TABLE IF NOT EXISTS issues (
    crawl_id INTEGER NOT NULL REFERENCES crawls(id),
    url_id INTEGER NOT NULL REFERENCES urls(id),
    code TEXT NOT NULL,
    detail TEXT
);
CREATE INDEX IF NOT EXISTS issues_code ON issues (crawl_id, code);
"#;

/// Indexes for looking up the links to or from one URL. Nothing a crawl,
/// its checks or a whole export does needs them (those read the table in
/// one pass), and on 5.7 million links they take 10 s to build: they are
/// built by whatever first asks a per-URL question.
const READ_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS links_to ON links (crawl_id, to_url);
CREATE INDEX IF NOT EXISTS links_from ON links (from_url_id);
"#;

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

pub struct Store {
    conn: Connection,
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

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.execute_batch(SCHEMA).context("create schema")?;
        // Files from before `crawls.stopped`.
        let stopped: i64 =
            conn.query_row("SELECT count(*) FROM pragma_table_info('crawls') WHERE name = 'stopped'", [], |row| row.get(0))?;
        if stopped == 0 {
            conn.execute_batch("ALTER TABLE crawls ADD COLUMN stopped INTEGER NOT NULL DEFAULT 0;")?;
        }
        Ok(Store { conn })
    }

    /// Opens a crawl file for reading only, alongside a crawl writing to
    /// it (WAL lets readers in while the writer commits).
    pub fn open_read(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open {} for reading", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Store { conn })
    }

    pub fn start_crawl(&self, start_url: &str, config_json: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO crawls (start_url, config) VALUES (?1, ?2)",
            params![start_url, config_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Closes a crawl; `stopped` when it was asked to stop with URLs still
    /// queued.
    pub fn finish_crawl(&self, crawl_id: i64, stopped: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE crawls SET finished_at = datetime('now'), stopped = ?2 WHERE id = ?1",
            params![crawl_id, stopped],
        )?;
        Ok(())
    }

    /// Opens a closed or interrupted crawl again, to go on with it under
    /// `config_json` (its settings, perhaps with a higher limit).
    pub fn reopen_crawl(&self, crawl_id: i64, config_json: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE crawls SET finished_at = NULL, stopped = 0, config = ?2 WHERE id = ?1",
            params![crawl_id, config_json],
        )?;
        anyhow::ensure!(changed == 1, "no crawl {crawl_id}");
        Ok(())
    }

    /// The URLs a crawl has written, in id order (which is the order it
    /// found them), without their facts.
    pub fn saved_urls(&self, crawl_id: i64) -> Result<Vec<SavedUrl>> {
        let mut select = self.conn.prepare(
            "SELECT id, url, internal, depth, status, content_type, error, blocked_by_robots
             FROM urls WHERE crawl_id = ?1 ORDER BY id",
        )?;
        let urls = select
            .query_map(params![crawl_id], |row| {
                Ok(SavedUrl {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    internal: row.get(2)?,
                    depth: row.get(3)?,
                    status: row.get(4)?,
                    content_type: row.get(5)?,
                    error: row.get(6)?,
                    blocked_by_robots: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(urls)
    }

    /// Builds the indexes for per-URL link lookups, if missing.
    pub fn ensure_read_indexes(&self) -> Result<()> {
        self.conn.execute_batch(READ_INDEXES).context("create indexes")?;
        Ok(())
    }

    /// The settings a crawl ran with, as JSON.
    pub fn crawl_config(&self, crawl_id: i64) -> Result<String> {
        Ok(self.conn.query_row("SELECT config FROM crawls WHERE id = ?1", params![crawl_id], |row| row.get(0))?)
    }

    /// The most recent crawl in the file.
    pub fn last_crawl(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT max(id) FROM crawls", [], |row| row.get(0))
            .optional()?
            .flatten())
    }

    /// Turns off foreign key checks for this connection: the crawl's writer
    /// only inserts rows whose references it made itself, and the lookups
    /// cost a third of a link row's insert.
    pub fn bulk_writes(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
        Ok(())
    }

    /// The first URL id no row uses yet; a crawl numbers its URLs from it.
    pub fn next_url_id(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT coalesce(max(id), 0) + 1 FROM urls", [], |row| row.get(0))?)
    }

    /// Applies `writes` in one transaction.
    pub fn apply(&mut self, crawl_id: i64, writes: &[Write]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut response = tx.prepare_cached(
                // A continued crawl requests again a page whose facts its first
                // run never wrote, under the same id.
                "INSERT OR REPLACE INTO urls (id, crawl_id, url, status, location, content_type, ttfb_ms, size, error, headers, internal, depth)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            let mut facts_row = tx.prepare_cached(
                "INSERT OR REPLACE INTO facts
                   (url_id, source, title, description, h1, canonical, robots, word_count, text_hash, links, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            let mut link_row = tx.prepare_cached(
                "INSERT INTO links (crawl_id, from_url_id, source, to_url, anchor, rel, nofollow, position, image)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut render_row = tx.prepare_cached(
                "INSERT OR REPLACE INTO renders
                   (url_id, final_url, error, dcl_ms, total_ms, settled, load_fired, console_errors, resources)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut body_row = tx.prepare_cached(
                "INSERT OR REPLACE INTO bodies (url_id, kind, encoding, size, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut issue_row =
                tx.prepare_cached("INSERT INTO issues (crawl_id, url_id, code, detail) VALUES (?1, ?2, ?3, ?4)")?;
            for write in writes {
                match write {
                    Write::Response { id, http, internal, depth } => {
                        response.execute(params![
                            id,
                            crawl_id,
                            http.url,
                            http.status,
                            http.location,
                            http.content_type,
                            http.ttfb_ms,
                            http.size as i64,
                            http.error,
                            serde_json::to_string(&http.headers)?,
                            internal,
                            depth,
                        ])?;
                    }
                    Write::Facts { url_id, facts, data, links } => {
                        let source = facts.source.as_str();
                        facts_row.execute(params![
                            url_id,
                            source,
                            facts.title(),
                            facts.description(),
                            facts.h1().next(),
                            facts.canonicals.first(),
                            (!facts.meta_robots.is_empty()).then(|| facts.meta_robots.join(", ")),
                            facts.word_count as i64,
                            format!("{:016x}", facts.text_hash),
                            facts.links.len() as i64,
                            data,
                        ])?;
                        if *links {
                            for link in &facts.links {
                                link_row.execute(params![
                                    crawl_id,
                                    url_id,
                                    source,
                                    link.href,
                                    link.anchor,
                                    link.rel.join(" "),
                                    link.nofollow,
                                    link.position.as_str(),
                                    link.image,
                                ])?;
                            }
                        }
                    }
                    Write::Render { url_id, render } => {
                        render_row.execute(params![
                            url_id,
                            render.final_url,
                            render.error,
                            render.dcl_ms as i64,
                            render.total_ms as i64,
                            render.settled,
                            render.load_fired,
                            render.console_errors_json,
                            render.resources_json,
                        ])?;
                    }
                    Write::Body { url_id, kind, encoding, size, data } => {
                        body_row.execute(params![url_id, kind.as_str(), encoding, *size as i64, data])?;
                    }
                    Write::Issues { url_id, issues } => {
                        for (code, detail) in issues {
                            issue_row.execute(params![crawl_id, url_id, code, detail])?;
                        }
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// A stored document as written, with its encoding.
    pub fn body(&self, url_id: i64, kind: BodyKind) -> Result<Option<(String, Vec<u8>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT encoding, data FROM bodies WHERE url_id = ?1 AND kind = ?2",
                params![url_id, kind.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Sets (url id, click depth, listed in a sitemap, blocked by robots.txt)
    /// for each URL.
    pub fn set_crawl_states(&mut self, states: &[(i64, Option<u32>, bool, bool)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut update = tx.prepare_cached(
                "UPDATE urls SET depth = ?2, in_sitemap = ?3, blocked_by_robots = ?4 WHERE id = ?1",
            )?;
            for (url_id, depth, in_sitemap, blocked) in states {
                update.execute(params![url_id, depth, in_sitemap, blocked])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replaces the crawl's issues.
    pub fn save_issues(&mut self, crawl_id: i64, issues: &[(i64, &str, Option<String>)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM issues WHERE crawl_id = ?1", params![crawl_id])?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO issues (crawl_id, url_id, code, detail) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (url_id, code, detail) in issues {
                insert.execute(params![crawl_id, url_id, code, detail])?;
            }
        }
        tx.commit()?;
        Ok(())
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
        let mut select = self.conn.prepare(&format!("{SELECT_URL} WHERE u.crawl_id = ?1 ORDER BY u.id"))?;
        let mut rows = select.query(params![crawl_id])?;
        let mut batch = Vec::with_capacity(chunk);
        while let Some(row) = rows.next()? {
            batch.push(encoded_url(row)?);
            if batch.len() >= chunk {
                each(std::mem::replace(&mut batch, Vec::with_capacity(chunk)))?;
            }
        }
        if !batch.is_empty() {
            each(batch)?;
        }
        Ok(())
    }

    /// One URL with its facts decoded.
    pub fn url(&self, id: i64) -> Result<Option<StoredUrl>> {
        let mut select = self.conn.prepare_cached(&format!("{SELECT_URL} WHERE u.id = ?1"))?;
        let mut rows = select.query(params![id])?;
        rows.next()?.map(|row| encoded_url(row)?.decode()).transpose()
    }

    /// Raw access for exports and ad-hoc queries.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// A URL with its documents' facts and render, as `encoded_url` reads it.
pub(crate) const SELECT_URL: &str = "SELECT u.id, u.url, u.status, u.location, u.content_type, u.ttfb_ms, u.size, u.error, u.headers,
        u.depth, u.in_sitemap, u.blocked_by_robots, u.internal, fw.data, fr.data,
        r.url_id, r.error, r.settled, r.console_errors, r.resources
    FROM urls u
    LEFT JOIN facts fw ON fw.url_id = u.id AND fw.source = 'raw'
    LEFT JOIN facts fr ON fr.url_id = u.id AND fr.source = 'rendered'
    LEFT JOIN renders r ON r.url_id = u.id";

pub(crate) fn encoded_url(row: &rusqlite::Row<'_>) -> Result<EncodedUrl> {
    let headers: String = row.get(8)?;
    Ok(EncodedUrl {
        url: StoredUrl {
            id: row.get(0)?,
            http: HttpInfo {
                url: row.get(1)?,
                status: row.get(2)?,
                location: row.get(3)?,
                content_type: row.get(4)?,
                ttfb_ms: row.get(5)?,
                size: row.get::<_, i64>(6)? as u64,
                error: row.get(7)?,
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
        raw: row.get(13)?,
        rendered: row.get(14)?,
        render: match row.get::<_, Option<i64>>(15)? {
            Some(_) => Some((row.get(16)?, row.get(17)?, row.get(18)?, row.get(19)?)),
            None => None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use seo_page::Source;

    #[test]
    fn round_trips_a_page() {
        let dir = std::env::temp_dir().join(format!("seo-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sqlite");
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        let crawl = store.start_crawl("https://example.com/", "{}").unwrap();
        let http = HttpInfo {
            url: "https://example.com/".into(),
            status: 200,
            content_type: Some("text/html".into()),
            headers: vec![("x-robots-tag".into(), "noindex".into())],
            ..Default::default()
        };
        let id = store.next_url_id().unwrap();
        let facts = seo_page::extract(
            "<title>T</title><a href='/b'>b</a>",
            &url::Url::parse("https://example.com/").unwrap(),
            Source::Raw,
        );
        let data = encode_facts(&facts, 3).unwrap();
        assert!(data.starts_with(&ZSTD_MAGIC));
        assert_eq!(decode_facts(&serde_json::to_vec(&facts).unwrap()).unwrap().title(), Some("T"));
        store
            .apply(
                crawl,
                &[
                    Write::Response { id, http, internal: true, depth: Some(0) },
                    Write::Facts { url_id: id, facts: Box::new(facts), data, links: true },
                    Write::Body { url_id: id, kind: BodyKind::Raw, encoding: "identity", size: 2, data: b"ok".to_vec() },
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
        assert_eq!(urls[0].raw.as_ref().unwrap().title(), Some("T"));
        assert_eq!(urls[0].depth, Some(0));
        let links: i64 = store
            .connection()
            .query_row("SELECT count(*) FROM links", [], |row| row.get(0))
            .unwrap();
        assert_eq!(links, 1);
    }
}
