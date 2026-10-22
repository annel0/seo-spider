//! Reads for an interactive view of a crawl: pages of rows sorted, filtered
//! and searched by the database, counts for the filters, the issues of a
//! crawl and of one URL. Safe on a read-only connection next to a running
//! crawl.

use anyhow::{bail, Result};
use rusqlite::{params, params_from_iter, types::Value};
use serde::Serialize;

use crate::Store;

/// A crawl in the file.
#[derive(Debug, Clone, Serialize)]
pub struct CrawlInfo {
    pub id: i64,
    pub start_url: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// URLs recorded so far.
    pub urls: i64,
    /// Stopped with URLs still queued: it can be continued.
    pub stopped: bool,
}

/// A crawl at a glance, for the overview's charts: counts by group.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Overview {
    /// Internal URLs.
    pub urls: i64,
    /// Internal HTML documents.
    pub pages: i64,
    /// 2xx, 3xx, 4xx, 5xx, no_response, blocked.
    pub statuses: Vec<(String, i64)>,
    /// html, css, js, image, font, pdf, data, other, none.
    pub content_types: Vec<(String, i64)>,
    /// HTML pages by clicks from the start.
    pub depths: Vec<(i64, i64)>,
    /// Responses by time to first byte: upper bound in ms (0 for over 2 s).
    pub response_times: Vec<(i64, i64)>,
    /// HTML pages: indexable, noindex, canonicalised, redirect, error, blocked.
    pub indexability: Vec<(String, i64)>,
    pub average_ttfb_ms: f64,
    pub bytes: i64,
}

/// One row of a table: a URL with its facts decoded, and how many issues
/// it has.
#[derive(Debug, Clone)]
pub struct StoredRow {
    pub url: crate::StoredUrl,
    pub issues: i64,
}

/// What `url_rows` returns: the rows asked for and how many match.
#[derive(Debug, Clone)]
pub struct RowsPage {
    pub total: i64,
    pub rows: Vec<StoredRow>,
}

/// Which rows, in what order.
#[derive(Debug, Clone)]
pub struct RowQuery {
    /// A key of `SCOPES`: the URLs a table lists at all.
    pub scope: String,
    /// A key of `FILTERS`, or `issue:<code>` for the URLs with that issue.
    pub filter: String,
    /// Part of the address, any case.
    pub search: Option<String>,
    /// A sortable column (see `sort_expression`).
    pub sort: String,
    pub descending: bool,
    pub offset: i64,
    pub limit: i64,
}

const HTML: &str = "(lower(u.content_type) LIKE 'text/html%' OR lower(u.content_type) LIKE 'application/xhtml%')";

/// The URLs a table lists: key and SQL condition.
pub const SCOPES: &[(&str, &str)] = &[
    ("all", "1"),
    ("internal", "u.internal = 1"),
    ("external", "u.internal = 0"),
    ("pages", "u.internal = 1 AND (lower(u.content_type) LIKE 'text/html%' OR lower(u.content_type) LIKE 'application/xhtml%')"),
    ("images", "lower(u.content_type) LIKE 'image/%'"),
];

/// Filters within a table: key and SQL condition.
pub const FILTERS: &[(&str, &str)] = &[
    ("all", "1"),
    ("html", HTML),
    ("js", "(lower(u.content_type) LIKE '%javascript%' OR lower(u.content_type) LIKE '%ecmascript%')"),
    ("css", "lower(u.content_type) LIKE 'text/css%'"),
    ("image", "lower(u.content_type) LIKE 'image/%'"),
    ("pdf", "lower(u.content_type) LIKE 'application/pdf%'"),
    ("font", "(lower(u.content_type) LIKE 'font/%' OR lower(u.content_type) LIKE '%font%')"),
    ("data", "(lower(u.content_type) LIKE '%json%' OR lower(u.content_type) LIKE '%xml%')"),
    ("2xx", "u.status BETWEEN 200 AND 299"),
    ("3xx", "u.status BETWEEN 300 AND 399"),
    ("4xx", "u.status BETWEEN 400 AND 499"),
    ("5xx", "u.status BETWEEN 500 AND 599"),
    ("no_response", "u.status = 0 AND u.blocked_by_robots = 0"),
    ("blocked", "u.blocked_by_robots = 1"),
    ("internal", "u.internal = 1"),
    ("external", "u.internal = 0"),
    ("noindex", "lower(coalesce(CASE WHEN fr.url_id IS NULL THEN fw.robots ELSE fr.robots END, '')) LIKE '%noindex%'"),
    ("in_sitemap", "u.in_sitemap = 1"),
    ("rendered", "fr.url_id IS NOT NULL"),
    ("large", "u.size > 102400"),
    ("slow", "u.status > 0 AND u.ttfb_ms >= 1000"),
];

/// The SQL expression a sortable column orders by.
fn sort_expression(key: &str) -> Option<String> {
    Some(match key {
        "url" | "status" | "content_type" | "depth" | "ttfb_ms" | "size" => format!("u.{key}"),
        "title" | "description" | "h1" | "word_count" | "links" | "canonical" => effective_value(key),
        "title_length" | "description_length" | "h1_length" => {
            format!("length({})", effective_value(key.trim_end_matches("_length")))
        }
        _ => return None,
    })
}

/// The effective document's facts: rendered when there is one.
const ROWS_FROM: &str = "FROM urls u
    LEFT JOIN facts fw ON fw.url_id = u.id AND fw.source = 'raw'
    LEFT JOIN facts fr ON fr.url_id = u.id AND fr.source = 'rendered'";

fn effective_value(column: &str) -> String {
    format!("CASE WHEN fr.url_id IS NULL THEN fw.{column} ELSE fr.{column} END")
}

impl Store {
    /// Every crawl in the file, newest first.
    pub fn crawls(&self) -> Result<Vec<CrawlInfo>> {
        let mut select = self.conn.prepare(
            "SELECT c.id, c.start_url, c.started_at, c.finished_at,
                    (SELECT count(*) FROM urls u WHERE u.crawl_id = c.id), c.stopped
             FROM crawls c ORDER BY c.id DESC",
        )?;
        let crawls = select
            .query_map([], |row| {
                Ok(CrawlInfo {
                    id: row.get(0)?,
                    start_url: row.get(1)?,
                    started_at: row.get(2)?,
                    finished_at: row.get(3)?,
                    urls: row.get(4)?,
                    stopped: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(crawls)
    }

    /// A page of a table: the crawl's URLs in the query's scope and filter.
    pub fn url_rows(&self, crawl_id: i64, query: &RowQuery) -> Result<RowsPage> {
        let Some((_, scope)) = SCOPES.iter().find(|(key, _)| *key == query.scope) else {
            bail!("unknown scope {:?}", query.scope);
        };
        let mut conditions = vec!["u.crawl_id = ?1".to_string(), scope.to_string()];
        let mut values: Vec<Value> = vec![Value::Integer(crawl_id)];
        match query.filter.strip_prefix("issue:") {
            Some(code) => {
                values.push(Value::Text(code.to_string()));
                conditions.push(format!(
                    "u.id IN (SELECT url_id FROM issues WHERE crawl_id = ?1 AND code = ?{})",
                    values.len()
                ));
            }
            None => match FILTERS.iter().find(|(key, _)| *key == query.filter) {
                Some((_, condition)) => conditions.push(condition.to_string()),
                None => bail!("unknown filter {:?}", query.filter),
            },
        }
        if let Some(search) = query.search.as_deref().map(str::trim).filter(|search| !search.is_empty()) {
            let escaped = search.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            values.push(Value::Text(format!("%{escaped}%")));
            conditions.push(format!("u.url LIKE ?{} ESCAPE '\\'", values.len()));
        }
        let Some(order) = sort_expression(&query.sort) else {
            bail!("unknown sort {:?}", query.sort);
        };
        let direction = if query.descending { "DESC" } else { "ASC" };
        let filter = conditions.join(" AND ");

        let total: i64 =
            self.conn.query_row(&format!("SELECT count(*) {ROWS_FROM} WHERE {filter}"), params_from_iter(&values), |row| {
                row.get(0)
            })?;
        let sql = format!(
            "{} WHERE {filter} ORDER BY {order} IS NULL, {order} {direction}, u.id LIMIT {} OFFSET {}",
            crate::SELECT_URL,
            query.limit.clamp(1, 5000),
            query.offset.max(0)
        );
        let mut select = self.conn.prepare(&sql)?;
        let mut found = select.query(params_from_iter(&values))?;
        let mut rows = Vec::new();
        while let Some(row) = found.next()? {
            rows.push(StoredRow { url: crate::encoded_url(row)?.decode()?, issues: 0 });
        }
        if !rows.is_empty() {
            let ids = rows.iter().map(|row| row.url.id.to_string()).collect::<Vec<_>>().join(",");
            let mut select =
                self.conn.prepare(&format!("SELECT url_id, count(*) FROM issues WHERE url_id IN ({ids}) GROUP BY url_id"))?;
            let counts: std::collections::HashMap<i64, i64> = select
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for row in &mut rows {
                row.issues = counts.get(&row.url.id).copied().unwrap_or(0);
            }
        }
        Ok(RowsPage { total, rows })
    }

    /// How many URLs each (scope, filter) pair keeps, in one pass over the
    /// crawl: keys of `SCOPES` and `FILTERS`.
    pub fn filter_counts(&self, crawl_id: i64, pairs: &[(&str, &str)]) -> Result<Vec<i64>> {
        let mut sums = Vec::with_capacity(pairs.len());
        for (scope, filter) in pairs {
            let Some((_, scope)) = SCOPES.iter().find(|(key, _)| key == scope) else {
                bail!("unknown scope {scope:?}");
            };
            let Some((_, filter)) = FILTERS.iter().find(|(key, _)| key == filter) else {
                bail!("unknown filter {filter:?}");
            };
            sums.push(format!("coalesce(sum(CASE WHEN ({scope}) AND ({filter}) THEN 1 ELSE 0 END), 0)"));
        }
        if sums.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!("SELECT {} {ROWS_FROM} WHERE u.crawl_id = ?1", sums.join(", "));
        self.conn
            .query_row(&sql, params![crawl_id], |row| (0..pairs.len()).map(|at| row.get(at)).collect())
            .map_err(Into::into)
    }

    /// Counts by group for the overview, over the crawl's internal URLs.
    pub fn overview(&self, crawl_id: i64) -> Result<Overview> {
        let html = "lower(u.content_type) LIKE 'text/html%'";
        let grouped = |sql: &str| -> Result<Vec<(String, i64)>> {
            let mut select = self.conn.prepare(sql)?;
            let rows = select
                .query_map(params![crawl_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        };
        let numbered = |sql: &str| -> Result<Vec<(i64, i64)>> {
            let mut select = self.conn.prepare(sql)?;
            let rows = select
                .query_map(params![crawl_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        };
        let (urls, pages, average_ttfb_ms, bytes) = self.conn.query_row(
            &format!(
                "SELECT count(*), coalesce(sum({html}), 0), coalesce(avg(CASE WHEN status > 0 THEN ttfb_ms END), 0),
                        coalesce(sum(size), 0)
                 FROM urls u WHERE crawl_id = ?1 AND internal = 1"
            ),
            params![crawl_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let statuses = grouped(
            "SELECT CASE WHEN blocked_by_robots = 1 THEN 'blocked' WHEN status = 0 THEN 'no_response'
                         ELSE (status / 100) || 'xx' END AS class, count(*)
             FROM urls WHERE crawl_id = ?1 AND internal = 1 GROUP BY class ORDER BY class",
        )?;
        let content_types = grouped(
            "SELECT CASE
                 WHEN content_type IS NULL THEN 'none'
                 WHEN lower(content_type) LIKE 'text/html%' OR lower(content_type) LIKE 'application/xhtml%' THEN 'html'
                 WHEN lower(content_type) LIKE 'text/css%' THEN 'css'
                 WHEN lower(content_type) LIKE '%javascript%' OR lower(content_type) LIKE '%ecmascript%' THEN 'js'
                 WHEN lower(content_type) LIKE 'image/%' THEN 'image'
                 WHEN lower(content_type) LIKE 'font/%' OR lower(content_type) LIKE '%font%' THEN 'font'
                 WHEN lower(content_type) LIKE 'application/pdf%' THEN 'pdf'
                 WHEN lower(content_type) LIKE '%json%' OR lower(content_type) LIKE '%xml%' THEN 'data'
                 ELSE 'other' END AS kind, count(*)
             FROM urls WHERE crawl_id = ?1 AND internal = 1 GROUP BY kind ORDER BY count(*) DESC",
        )?;
        let depths = numbered(&format!(
            "SELECT coalesce(depth, -1) AS d, count(*) FROM urls u
             WHERE crawl_id = ?1 AND internal = 1 AND {html} GROUP BY d ORDER BY d"
        ))?;
        let response_times = numbered(
            "SELECT CASE WHEN ttfb_ms < 100 THEN 100 WHEN ttfb_ms < 300 THEN 300 WHEN ttfb_ms < 600 THEN 600
                         WHEN ttfb_ms < 1000 THEN 1000 WHEN ttfb_ms < 2000 THEN 2000 ELSE 0 END AS bucket, count(*)
             FROM urls WHERE crawl_id = ?1 AND internal = 1 AND status > 0 GROUP BY bucket",
        )?;
        let robots = "lower(coalesce(CASE WHEN fr.url_id IS NULL THEN fw.robots ELSE fr.robots END, ''))";
        let canonical = "CASE WHEN fr.url_id IS NULL THEN fw.canonical ELSE fr.canonical END";
        let indexability = grouped(&format!(
            "SELECT CASE
                 WHEN u.blocked_by_robots = 1 THEN 'blocked'
                 WHEN u.status BETWEEN 300 AND 399 THEN 'redirect'
                 WHEN u.status = 0 OR u.status >= 400 THEN 'error'
                 WHEN {robots} LIKE '%noindex%' THEN 'noindex'
                 WHEN coalesce({canonical}, '') NOT IN ('', u.url) THEN 'canonicalised'
                 ELSE 'indexable' END AS state, count(*)
             {ROWS_FROM} WHERE u.crawl_id = ?1 AND u.internal = 1
               AND ({html} OR u.blocked_by_robots = 1 OR u.status = 0 OR u.status BETWEEN 300 AND 399)
             GROUP BY state ORDER BY count(*) DESC"
        ))?;
        Ok(Overview {
            urls,
            pages,
            statuses,
            content_types,
            depths,
            response_times,
            indexability,
            average_ttfb_ms,
            bytes,
        })
    }

    /// How many URLs have each issue code.
    pub fn issue_counts(&self, crawl_id: i64) -> Result<Vec<(String, i64)>> {
        let mut select = self.conn.prepare_cached(
            "SELECT code, count(DISTINCT url_id) AS urls FROM issues WHERE crawl_id = ?1 GROUP BY code ORDER BY urls DESC",
        )?;
        let counts = select
            .query_map(params![crawl_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(counts)
    }

    /// The issues of one URL: code and detail.
    pub fn url_issues(&self, url_id: i64) -> Result<Vec<(String, Option<String>)>> {
        let mut select = self.conn.prepare_cached("SELECT code, detail FROM issues WHERE url_id = ?1")?;
        let issues = select
            .query_map(params![url_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(issues)
    }

    /// Indexes the view's per-URL lookups use. Not made by a crawl: they
    /// cost its writes and only a view reads them.
    pub fn ensure_view_indexes(&self) -> Result<()> {
        self.conn.execute_batch("CREATE INDEX IF NOT EXISTS issues_url ON issues (url_id);")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Write;
    use seo_page::{HttpInfo, Source};

    fn page(url: &str, title: Option<&str>, status: u16) -> (HttpInfo, Option<seo_page::PageFacts>) {
        let http = HttpInfo {
            url: url.into(),
            status,
            content_type: Some("text/html; charset=utf-8".into()),
            ..Default::default()
        };
        let html = match title {
            Some(title) => format!("<html><head><title>{title}</title></head><body><h1>{title}</h1></body></html>"),
            None => "<html><body>no title</body></html>".into(),
        };
        let facts = (status == 200).then(|| seo_page::extract(&html, &url::Url::parse(url).unwrap(), Source::Raw));
        (http, facts)
    }

    #[test]
    fn rows_are_filtered_sorted_searched_and_counted() {
        let path = std::env::temp_dir().join(format!("seo-store-view-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        let crawl = store.start_crawl("https://example.com/", "{}").unwrap();
        let mut writes = Vec::new();
        for (id, (url, title, status)) in [
            ("https://example.com/", Some("Home"), 200),
            ("https://example.com/about", Some("About us and the long story"), 200),
            ("https://example.com/gone", None, 404),
            ("https://example.com/untitled", None, 200),
        ]
        .into_iter()
        .enumerate()
        {
            let (http, facts) = page(url, title, status);
            writes.push(Write::Response { id: id as i64 + 1, http, internal: true, depth: Some(id as u32) });
            if let Some(facts) = facts {
                let data = crate::encode_facts(&facts, 3).unwrap();
                writes.push(Write::Facts { url_id: id as i64 + 1, facts: Box::new(facts), data, links: false });
            }
        }
        writes.push(Write::Issues { url_id: 4, issues: vec![("title_missing", None), ("h1_missing", None)] });
        store.apply(crawl, &writes).unwrap();
        store.ensure_view_indexes().unwrap();
        drop(store);

        let store = Store::open_read(&path).unwrap();
        let query = |scope: &str, filter: &str, sort: &str, descending: bool, search: Option<&str>| RowQuery {
            scope: scope.into(),
            filter: filter.into(),
            search: search.map(Into::into),
            sort: sort.into(),
            descending,
            offset: 0,
            limit: 100,
        };
        let title = |row: &StoredRow| row.url.facts().and_then(|facts| facts.title()).map(str::to_string);
        let page = store.url_rows(crawl, &query("internal", "all", "title_length", true, None)).unwrap();
        assert_eq!(page.total, 4);
        // Longest title first; rows without a title last.
        assert_eq!(page.rows[0].url.http.url, "https://example.com/about");
        assert!(title(&page.rows[2]).is_none() && title(&page.rows[3]).is_none());
        let errors = store.url_rows(crawl, &query("internal", "4xx", "url", false, None)).unwrap();
        assert_eq!(errors.rows.iter().map(|row| row.url.http.url.as_str()).collect::<Vec<_>>(), ["https://example.com/gone"]);
        let missing = store.url_rows(crawl, &query("pages", "issue:title_missing", "url", false, None)).unwrap();
        assert_eq!(missing.total, 1);
        assert_eq!(missing.rows[0].issues, 2);
        let found = store.url_rows(crawl, &query("internal", "all", "url", false, Some("ABOUT"))).unwrap();
        assert_eq!(found.total, 1);
        assert_eq!(store.url_rows(crawl, &query("external", "all", "url", false, None)).unwrap().total, 0);
        let counts = store.filter_counts(crawl, &[("internal", "all"), ("internal", "2xx"), ("internal", "4xx"), ("external", "all")]).unwrap();
        assert_eq!(counts, [4, 3, 1, 0]);
        assert!(store.filter_counts(crawl, &[("internal", "nope")]).is_err());
        assert!(store.url_rows(crawl, &query("internal", "nope", "url", false, None)).is_err());
        assert!(store.url_rows(crawl, &query("nowhere", "all", "url", false, None)).is_err());
        assert!(store.url_rows(crawl, &query("internal", "all", "url; DROP TABLE urls", false, None)).is_err());
        assert_eq!(store.issue_counts(crawl).unwrap().len(), 2);
        let overview = store.overview(crawl).unwrap();
        assert_eq!((overview.urls, overview.pages), (4, 4));
        assert!(overview.statuses.contains(&("2xx".into(), 3)) && overview.statuses.contains(&("4xx".into(), 1)));
        assert_eq!(overview.depths, [(0, 1), (1, 1), (2, 1), (3, 1)]);
        assert!(overview.indexability.contains(&("indexable".into(), 3)));
        assert!(overview.indexability.contains(&("error".into(), 1)));
        assert_eq!(overview.content_types, [("html".to_string(), 4)]);
        assert_eq!(store.url_issues(4).unwrap().len(), 2);
        assert_eq!(store.crawls().unwrap()[0].urls, 4);
        let one = store.url(2).unwrap().unwrap();
        assert_eq!(one.raw.unwrap().title(), Some("About us and the long story"));
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
