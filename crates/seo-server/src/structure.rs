//! The site's structure: its internal URLs as a tree of path segments
//! (`/catalog/notebooks/a5/`), each section with how many URLs it holds,
//! how many answered with an error, have issues (of high importance), and
//! their best link score, for a treemap. Big sections keep their largest parts and fold
//! the rest into one.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::Json;
use seo_store::{params, Store};
use serde::{Deserialize, Serialize};

use crate::{read, ApiResult, AppState};

/// Sections below this many segments are folded into their parent.
const DEPTH: usize = 6;
/// Parts a section shows; the rest become one (a treemap draws a few
/// hundred cells well, not a catalog's ten thousand).
const PARTS: usize = 150;

#[derive(Default)]
struct Tree {
    /// The segment as the URLs have it (percent-escaped).
    raw: String,
    stats: Stats,
    /// URLs that end here (the section's own page, its query variants).
    own: Stats,
    children: BTreeMap<String, Tree>,
}

#[derive(Default, Clone, Copy, Serialize)]
pub(crate) struct Stats {
    urls: u32,
    /// HTML pages.
    pages: u32,
    /// 4xx, 5xx, no response.
    errors: u32,
    redirects: u32,
    /// URLs with at least one issue.
    with_issues: u32,
    /// URLs with an issue of high importance.
    serious: u32,
    /// The highest link score in the section.
    best_score: Option<i64>,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.urls += other.urls;
        self.pages += other.pages;
        self.errors += other.errors;
        self.redirects += other.redirects;
        self.with_issues += other.with_issues;
        self.serious += other.serious;
        self.best_score = self.best_score.max(other.best_score);
    }
}

/// One section as the treemap takes it: `value` (URLs) on leaves only, so
/// a section is the sum of its parts.
#[derive(Serialize)]
pub(crate) struct Node {
    name: String,
    /// The path up to here, as people read it.
    path: String,
    /// The same as the URLs have it, to search the tables by.
    search: String,
    #[serde(flatten)]
    stats: Stats,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<Node>,
}

/// The path's segments: as written, and as people read them.
fn segments(address: &str) -> Vec<(String, String)> {
    let Ok(url) = url::Url::parse(address) else { return Vec::new() };
    url.path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .take(DEPTH)
        .map(|segment| (segment.to_string(), percent_decode(segment)))
        .collect()
}

/// `%D0%BA` → `к`, as people read paths; as is when it does not decode.
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[at + 1]), hex(bytes[at + 2])) {
                out.push((high * 16 + low) as u8);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| segment.to_string())
}

fn node(name: String, path: String, search: String, tree: Tree) -> Node {
    let Tree { stats, own, children, .. } = tree;
    if children.is_empty() {
        return Node { name, path, search, stats, value: Some(stats.urls), children: Vec::new() };
    }
    let mut parts: Vec<(String, Tree)> = children.into_iter().collect();
    parts.sort_by(|a, b| b.1.stats.urls.cmp(&a.1.stats.urls).then_with(|| a.0.cmp(&b.0)));
    let mut rest = Stats::default();
    let mut folded = 0;
    for (_, part) in parts.drain(PARTS.min(parts.len())..) {
        rest.add(&part.stats);
        folded += 1;
    }
    let mut nodes: Vec<Node> = parts
        .into_iter()
        .map(|(segment, part)| {
            let search = format!("{search}{}/", part.raw);
            node(segment.clone(), format!("{path}{segment}/"), search, part)
        })
        .collect();
    let leaf = |name: String, stats: Stats| Node {
        name,
        path: path.clone(),
        search: search.clone(),
        stats,
        value: Some(stats.urls),
        children: Vec::new(),
    };
    if own.urls > 0 {
        nodes.push(leaf("(сам раздел)".into(), own));
    }
    if folded > 0 {
        nodes.push(leaf(format!("ещё {folded}"), rest));
    }
    Node { name, path, search, stats, value: None, children: nodes }
}

/// The crawl's internal URLs (`pages`: HTML pages only) as a tree under
/// its host.
pub(crate) fn structure_of(store: &Store, crawl_id: i64, pages: bool) -> Result<Node> {
    // Per URL: has issues, has a serious one.
    let mut issues: HashMap<i64, bool> = HashMap::new();
    store.query("SELECT DISTINCT url_id, code FROM issues WHERE crawl_id = ?1", &params![crawl_id], |mut row| {
        let id: i64 = row.get(0)?;
        let serious = seo_checks::find(&row.take_string(1)?).is_some_and(|check| check.severity == seo_checks::Severity::High);
        *issues.entry(id).or_default() |= serious;
        Ok(())
    })?;
    let mut root = Tree::default();
    let mut host = String::new();
    let only = if pages {
        " AND (lower(u.content_type) LIKE 'text/html%' OR lower(u.content_type) LIKE 'application/xhtml%')"
    } else {
        ""
    };
    store.query(
        &format!(
            "SELECT u.id, u.url, u.status, u.content_type, ls.score FROM urls u
             LEFT JOIN link_scores ls ON ls.url_id = u.id
             WHERE u.crawl_id = ?1 AND u.internal = 1{only}"
        ),
        &params![crawl_id],
        |mut row| {
            let id: i64 = row.get(0)?;
            let address = row.take_string(1)?;
            let status: u16 = row.get(2)?;
            let html = row.take_text(3)?.is_some_and(|kind| {
                let kind = kind.to_ascii_lowercase();
                kind.starts_with("text/html") || kind.starts_with("application/xhtml")
            });
            let stats = Stats {
                urls: 1,
                pages: u32::from(html),
                errors: u32::from(status == 0 || status >= 400),
                redirects: u32::from((300..400).contains(&status)),
                with_issues: u32::from(issues.contains_key(&id)),
                serious: u32::from(issues.get(&id).copied().unwrap_or(false)),
                best_score: row.get(4)?,
            };
            if host.is_empty() {
                host = url::Url::parse(&address).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_default();
            }
            let mut at = &mut root;
            at.stats.add(&stats);
            for (raw, segment) in segments(&address) {
                at = at.children.entry(segment).or_insert_with(|| Tree { raw, ..Tree::default() });
                at.stats.add(&stats);
            }
            at.own.add(&stats);
            Ok(())
        },
    )?;
    Ok(node(host, "/".into(), "/".into(), root))
}

#[derive(Deserialize)]
pub(crate) struct Which {
    /// Every internal URL, not only HTML pages.
    all: Option<bool>,
}

pub(crate) async fn structure(State(state): State<Arc<AppState>>, Path(id): Path<i64>, Query(which): Query<Which>) -> ApiResult<Node> {
    let pages = !which.all.unwrap_or(false);
    Ok(Json(read(&state, move |store| structure_of(store, id, pages)).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_read_as_people_read_them() {
        let read: Vec<String> = segments("https://e.com/catalog/%D0%B1%D0%BB%D0%BE%D0%BA/a5?x=1").into_iter().map(|(_, read)| read).collect();
        assert_eq!(read, ["catalog", "блок", "a5"]);
        assert_eq!(segments("https://e.com/catalog/%D0%B1")[1].0, "%D0%B1");
        assert!(segments("https://e.com/").is_empty());
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn big_sections_fold_their_small_parts() {
        let page = |urls: u32| Tree {
            stats: Stats { urls, ..Stats::default() },
            own: Stats { urls, ..Stats::default() },
            ..Tree::default()
        };
        let mut root = Tree::default();
        for at in 0..(PARTS + 5) {
            let size = if at == 0 { 100 } else { 1 };
            root.stats.urls += size;
            root.children.insert(format!("p{at}"), page(size));
        }
        let built = node("e.com".into(), "/".into(), "/".into(), root);
        assert_eq!(built.value, None);
        assert_eq!(built.children.len(), PARTS + 1);
        assert_eq!((built.children[0].name.as_str(), built.children[0].value), ("p0", Some(100)));
        let folded = built.children.last().unwrap();
        assert_eq!((folded.name.as_str(), folded.value), ("ещё 5", Some(5)));
        let leaves: u32 = built.children.iter().filter_map(|child| child.value).sum();
        assert_eq!(leaves, built.stats.urls);
    }
}
