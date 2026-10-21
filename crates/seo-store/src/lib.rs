//! SQLite storage for crawls. One file holds any number of crawls; each URL
//! row carries its HTTP response, and raw and rendered facts are kept as JSON
//! next to the columns exports and queries need.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use seo_page::{HttpInfo, PageFacts, Source};

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
    config TEXT NOT NULL DEFAULT '{}'
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

/// Indexes a crawl does not need while it writes: built once when it is
/// finished instead of being kept up on every insert.
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
}

impl StoredUrl {
    /// Rendered facts when the page was rendered, raw ones otherwise.
    pub fn facts(&self) -> Option<&PageFacts> {
        self.rendered.as_ref().or(self.raw.as_ref())
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
    Response { id: i64, http: HttpInfo, internal: bool },
    /// Facts of one document of a URL, with `PageFacts` already encoded by
    /// `encode_facts`; `links` also stores its outgoing links as rows.
    Facts { url_id: i64, facts: Box<PageFacts>, data: Vec<u8>, links: bool },
    Render { url_id: i64, render: Box<RenderRecord> },
    /// A whole document; `encoding` is `identity` or `zstd`.
    Body { url_id: i64, kind: BodyKind, encoding: &'static str, size: usize, data: Vec<u8> },
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.execute_batch(SCHEMA).context("create schema")?;
        Ok(Store { conn })
    }

    pub fn start_crawl(&self, start_url: &str, config_json: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO crawls (start_url, config) VALUES (?1, ?2)",
            params![start_url, config_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_crawl(&self, crawl_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE crawls SET finished_at = datetime('now') WHERE id = ?1",
            params![crawl_id],
        )?;
        self.ensure_read_indexes()
    }

    /// Builds the indexes exports and checks query by, if missing.
    pub fn ensure_read_indexes(&self) -> Result<()> {
        self.conn.execute_batch(READ_INDEXES).context("create indexes")?;
        Ok(())
    }

    /// The most recent crawl in the file.
    pub fn last_crawl(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT max(id) FROM crawls", [], |row| row.get(0))
            .optional()?
            .flatten())
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
                "INSERT INTO urls (id, crawl_id, url, status, location, content_type, ttfb_ms, size, error, headers, internal)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
            for write in writes {
                match write {
                    Write::Response { id, http, internal } => {
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
        let mut select = self.conn.prepare(
            "SELECT id, url, status, location, content_type, ttfb_ms, size, error, headers,
                    depth, in_sitemap, blocked_by_robots, internal
             FROM urls WHERE crawl_id = ?1 ORDER BY id",
        )?;
        let mut rows = select.query(params![crawl_id])?;
        let mut urls = Vec::new();
        while let Some(row) = rows.next()? {
            let headers: String = row.get(8)?;
            urls.push(StoredUrl {
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
            });
        }
        let index: std::collections::HashMap<i64, usize> =
            urls.iter().enumerate().map(|(i, url)| (url.id, i)).collect();
        let mut select = self.conn.prepare(
            "SELECT f.url_id, f.source, f.data FROM facts f JOIN urls u ON u.id = f.url_id
             WHERE u.crawl_id = ?1",
        )?;
        let mut rows = select.query(params![crawl_id])?;
        while let Some(row) = rows.next()? {
            let url_id: i64 = row.get(0)?;
            let source: String = row.get(1)?;
            let data: Vec<u8> = row.get(2)?;
            let Some(&i) = index.get(&url_id) else { continue };
            let facts = decode_facts(&data)?;
            match facts.source {
                Source::Raw if source == "raw" => urls[i].raw = Some(facts),
                Source::Rendered if source == "rendered" => urls[i].rendered = Some(facts),
                _ => {}
            }
        }
        Ok(urls)
    }

    /// Raw access for exports and ad-hoc queries.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    Write::Response { id, http, internal: true },
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
