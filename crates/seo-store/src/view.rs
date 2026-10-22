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
}

/// One row of the internal URLs table: the response and the facts of the
/// document the page is judged by (rendered when it was rendered).
#[derive(Debug, Clone, Serialize)]
pub struct UrlRow {
    pub id: i64,
    pub url: String,
    pub status: i64,
    pub content_type: Option<String>,
    pub error: Option<String>,
    pub depth: Option<i64>,
    pub ttfb_ms: i64,
    pub size: i64,
    pub in_sitemap: bool,
    pub title: Option<String>,
    pub title_length: Option<i64>,
    pub description: Option<String>,
    pub description_length: Option<i64>,
    pub h1: Option<String>,
    pub h1_length: Option<i64>,
    pub word_count: Option<i64>,
    pub links: Option<i64>,
    pub canonical: Option<String>,
    pub robots: Option<String>,
    pub rendered: bool,
    /// Issues stored for the URL (live ones while the crawl runs).
    pub issues: i64,
}

/// What `url_rows` returns: the rows asked for and how many match.
#[derive(Debug, Clone, Serialize)]
pub struct RowsPage {
    pub total: i64,
    pub rows: Vec<UrlRow>,
}

/// Which rows, in what order.
#[derive(Debug, Clone)]
pub struct RowQuery {
    /// A key of `FILTERS`, or `issue:<code>` for the URLs with that issue.
    pub filter: String,
    /// Part of the address, any case.
    pub search: Option<String>,
    /// A key of `SORTS`.
    pub sort: String,
    pub descending: bool,
    pub offset: i64,
    pub limit: i64,
}

/// The filters of the internal table: key and SQL condition.
pub const FILTERS: &[(&str, &str)] = &[
    ("all", "1"),
    ("html", "lower(u.content_type) LIKE 'text/html%'"),
    ("2xx", "u.status BETWEEN 200 AND 299"),
    ("3xx", "u.status BETWEEN 300 AND 399"),
    ("4xx", "u.status BETWEEN 400 AND 499"),
    ("5xx", "u.status BETWEEN 500 AND 599"),
    ("no_response", "u.status = 0 AND u.blocked_by_robots = 0"),
    ("blocked", "u.blocked_by_robots = 1"),
    ("noindex", "lower(coalesce(CASE WHEN fr.url_id IS NULL THEN fw.robots ELSE fr.robots END, '')) LIKE '%noindex%'"),
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
                    (SELECT count(*) FROM urls u WHERE u.crawl_id = c.id)
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
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(crawls)
    }

    /// A page of the crawl's internal URLs.
    pub fn url_rows(&self, crawl_id: i64, query: &RowQuery) -> Result<RowsPage> {
        let mut conditions = vec!["u.crawl_id = ?1".to_string(), "u.internal = 1".to_string()];
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
        let columns = ["title", "description", "h1", "canonical", "robots", "word_count", "links"]
            .iter()
            .map(|column| effective_value(column))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT u.id, u.url, u.status, u.content_type, u.error, u.depth, u.ttfb_ms, u.size, u.in_sitemap,
                    {columns}, fr.url_id IS NOT NULL
             {ROWS_FROM} WHERE {filter}
             ORDER BY {order} IS NULL, {order} {direction}, u.id
             LIMIT {} OFFSET {}",
            query.limit.clamp(1, 1000),
            query.offset.max(0)
        );
        let mut select = self.conn.prepare(&sql)?;
        let mut rows: Vec<UrlRow> = select
            .query_map(params_from_iter(&values), |row| {
                let title: Option<String> = row.get(9)?;
                let description: Option<String> = row.get(10)?;
                let h1: Option<String> = row.get(11)?;
                let length = |text: &Option<String>| text.as_ref().map(|text| text.chars().count() as i64);
                Ok(UrlRow {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    status: row.get(2)?,
                    content_type: row.get(3)?,
                    error: row.get(4)?,
                    depth: row.get(5)?,
                    ttfb_ms: row.get(6)?,
                    size: row.get(7)?,
                    in_sitemap: row.get(8)?,
                    title_length: length(&title),
                    description_length: length(&description),
                    h1_length: length(&h1),
                    title,
                    description,
                    h1,
                    canonical: row.get(12)?,
                    robots: row.get(13)?,
                    word_count: row.get(14)?,
                    links: row.get(15)?,
                    rendered: row.get(16)?,
                    issues: 0,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if !rows.is_empty() {
            let ids = rows.iter().map(|row| row.id.to_string()).collect::<Vec<_>>().join(",");
            let mut select =
                self.conn.prepare(&format!("SELECT url_id, count(*) FROM issues WHERE url_id IN ({ids}) GROUP BY url_id"))?;
            let counts: std::collections::HashMap<i64, i64> = select
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for row in &mut rows {
                row.issues = counts.get(&row.id).copied().unwrap_or(0);
            }
        }
        Ok(RowsPage { total, rows })
    }

    /// How many internal URLs each filter of `FILTERS` keeps.
    pub fn filter_counts(&self, crawl_id: i64) -> Result<Vec<(&'static str, i64)>> {
        let sums = FILTERS
            .iter()
            .map(|(_, condition)| format!("coalesce(sum(CASE WHEN {condition} THEN 1 ELSE 0 END), 0)"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT {sums} {ROWS_FROM} WHERE u.crawl_id = ?1 AND u.internal = 1");
        self.conn.query_row(&sql, params![crawl_id], |row| {
            FILTERS.iter().enumerate().map(|(at, (key, _))| Ok((*key, row.get(at)?))).collect()
        })
        .map_err(Into::into)
    }

    /// How many URLs have each issue code.
    pub fn issue_counts(&self, crawl_id: i64) -> Result<Vec<(String, i64)>> {
        let mut select = self.conn.prepare_cached(
            "SELECT code, count(*) FROM issues WHERE crawl_id = ?1 GROUP BY code ORDER BY count(*) DESC",
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
        let query = |filter: &str, sort: &str, descending: bool, search: Option<&str>| RowQuery {
            filter: filter.into(),
            search: search.map(Into::into),
            sort: sort.into(),
            descending,
            offset: 0,
            limit: 100,
        };
        let page = store.url_rows(crawl, &query("all", "title_length", true, None)).unwrap();
        assert_eq!(page.total, 4);
        // Longest title first; rows without a title last.
        assert_eq!(page.rows[0].url, "https://example.com/about");
        assert_eq!(page.rows[0].title_length, Some(27));
        assert!(page.rows[2].title.is_none() && page.rows[3].title.is_none());
        let errors = store.url_rows(crawl, &query("4xx", "url", false, None)).unwrap();
        assert_eq!(errors.rows.iter().map(|row| row.url.as_str()).collect::<Vec<_>>(), ["https://example.com/gone"]);
        let missing = store.url_rows(crawl, &query("issue:title_missing", "url", false, None)).unwrap();
        assert_eq!(missing.total, 1);
        assert_eq!(missing.rows[0].issues, 2);
        let found = store.url_rows(crawl, &query("all", "url", false, Some("ABOUT"))).unwrap();
        assert_eq!(found.total, 1);
        let counts = store.filter_counts(crawl).unwrap();
        assert!(counts.contains(&("all", 4)) && counts.contains(&("2xx", 3)) && counts.contains(&("4xx", 1)));
        assert_eq!(store.issue_counts(crawl).unwrap().len(), 2);
        assert_eq!(store.url_issues(4).unwrap().len(), 2);
        assert!(store.url_rows(crawl, &query("nope", "url", false, None)).is_err());
        assert!(store.url_rows(crawl, &query("all", "url; DROP TABLE urls", false, None)).is_err());
        assert_eq!(store.crawls().unwrap()[0].urls, 4);
        let one = store.url(2).unwrap().unwrap();
        assert_eq!(one.raw.unwrap().title(), Some("About us and the long story"));
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
