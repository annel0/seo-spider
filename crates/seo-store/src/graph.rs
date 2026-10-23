//! The link graph between a crawl's URLs, written after its checks. Per
//! target URL, one row lists the pages that link or redirect to it, packed:
//! a site's pages link to the same targets over and over (a navigation of
//! a hundred links on each of 100,000 pages is ten million edges), and a
//! row per edge cost more to write than the checks took. It answers who
//! links to a URL and where the crawl's broken links sit; their anchors
//! stay in the source pages' facts, which a view reads for the rows it
//! shows.

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::schema::{INLINKS, LINK_SCORES};
use crate::{params, Param, Store};

/// How one URL reaches another.
pub const LINK: i64 = 0;
pub const REDIRECT: i64 = 1;

/// A page that links or redirects to a target: what the graph keeps per
/// link, 16 bytes, for millions of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    pub from_id: i64,
    /// Links on the page to the target (a redirect counts one).
    pub links: u32,
    pub redirect: bool,
    /// Every one of the links is nofollow.
    pub nofollow: bool,
}

/// An edge with both ends' addresses and the target's status.
#[derive(Debug, Clone, Serialize)]
pub struct LinkRow {
    pub from_id: i64,
    pub from_url: String,
    pub to_id: i64,
    pub to_url: String,
    pub status: u16,
    pub kind: i64,
    pub links: i64,
    pub nofollow: bool,
}

/// A target that answered with an error, or did not answer for a reason
/// other than robots.txt.
const BROKEN: &str = "(t.status >= 400 OR (t.status = 0 AND t.blocked_by_robots = 0))";

fn put(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn take(data: &[u8], at: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let Some(&byte) = data.get(*at) else { bail!("inlinks: cut short") };
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(value);
        }
    }
    bail!("inlinks: varint too long")
}

/// A target's sources, sorted by source id: the gap from the previous
/// source, then links << 2 | redirect << 1 | nofollow, each a varint.
fn pack(sources: &[Source]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sources.len() * 3);
    let mut previous = 0;
    for source in sources {
        put(&mut out, (source.from_id - previous) as u64);
        put(&mut out, u64::from(source.links) << 2 | u64::from(source.redirect) << 1 | u64::from(source.nofollow));
        previous = source.from_id;
    }
    out
}

fn unpack(data: &[u8]) -> Result<Vec<Source>> {
    let (mut at, mut from_id, mut sources) = (0, 0i64, Vec::new());
    while at < data.len() {
        from_id += take(data, &mut at)? as i64;
        let flags = take(data, &mut at)?;
        sources.push(Source { from_id, links: (flags >> 2) as u32, redirect: flags & 2 != 0, nofollow: flags & 1 != 0 });
    }
    Ok(sources)
}

impl Store {
    /// Replaces the crawl's link graph: each target URL's id and the pages
    /// that link or redirect to it.
    pub fn save_inlinks(&mut self, crawl_id: i64, targets: &mut [(i64, Vec<Source>)]) -> Result<()> {
        self.transaction(|db| {
            db.execute("DELETE FROM inlinks WHERE crawl_id = ?1", &params![crawl_id])?;
            for slice in targets.chunks_mut(16_384) {
                let packed: Vec<(i64, usize, Vec<u8>)> = slice
                    .iter_mut()
                    .filter(|(_, sources)| !sources.is_empty())
                    .map(|(to_id, sources)| {
                        sources.sort_unstable_by_key(|source| (source.from_id, source.redirect));
                        (*to_id, sources.len(), pack(sources))
                    })
                    .collect();
                let rows: Vec<Vec<Param>> =
                    packed.iter().map(|(to_id, count, data)| params![crawl_id, *to_id, *count, data].to_vec()).collect();
                db.insert_rows(&INLINKS, &rows)?;
            }
            Ok(())
        })
    }

    /// Whether the crawl's link graph and link scores were written (crawls
    /// checked before they existed have none).
    pub fn has_edges(&self, crawl_id: i64) -> Result<bool> {
        Ok(self.query_row("SELECT 1 FROM link_scores WHERE crawl_id = ?1 LIMIT 1", &params![crawl_id])?.is_some())
    }

    /// A URL's link score (internal PageRank, 0 to 100), once written.
    pub fn link_score(&self, url_id: i64) -> Result<Option<i64>> {
        match self.query_row("SELECT score FROM link_scores WHERE url_id = ?1", &params![url_id])? {
            Some(row) => row.get(0),
            None => Ok(None),
        }
    }

    /// Replaces the crawl's link scores: per URL, the pages linking to it
    /// and its internal PageRank as 0 to 100 (none for an external URL).
    pub fn save_link_scores(&mut self, crawl_id: i64, scores: &[(i64, i64, Option<i64>)]) -> Result<()> {
        self.transaction(|db| {
            db.execute("DELETE FROM link_scores WHERE crawl_id = ?1", &params![crawl_id])?;
            for slice in scores.chunks(16_384) {
                let rows: Vec<Vec<Param>> =
                    slice.iter().map(|(url_id, inlinks, score)| params![*url_id, crawl_id, *inlinks, *score].to_vec()).collect();
                db.insert_rows(&LINK_SCORES, &rows)?;
            }
            Ok(())
        })
    }

    /// The crawl a URL belongs to.
    pub fn url_crawl(&self, url_id: i64) -> Result<Option<i64>> {
        match self.query_row("SELECT crawl_id FROM urls WHERE id = ?1", &params![url_id])? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Addresses and statuses of URLs by id.
    fn addresses(&self, ids: &[i64]) -> Result<HashMap<i64, (String, u16)>> {
        let mut found = HashMap::new();
        for chunk in ids.chunks(500) {
            let list = chunk.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            self.query(&format!("SELECT id, url, status FROM urls WHERE id IN ({list})"), &[], |row| {
                found.insert(row.get::<i64>(0)?, (row.get(1)?, row.get(2)?));
                Ok(())
            })?;
        }
        Ok(found)
    }

    /// (target, source) pairs with the addresses of both ends and the
    /// target's status.
    fn link_rows(&self, edges: &[(i64, Source)], targets: &HashMap<i64, (String, u16)>) -> Result<Vec<LinkRow>> {
        let sources = self.addresses(&edges.iter().map(|(_, source)| source.from_id).collect::<Vec<_>>())?;
        Ok(edges
            .iter()
            .filter_map(|(to_id, source)| {
                let (from_url, _) = sources.get(&source.from_id)?;
                let (to_url, status) = targets.get(to_id)?;
                Some(LinkRow {
                    from_id: source.from_id,
                    from_url: from_url.clone(),
                    to_id: *to_id,
                    to_url: to_url.clone(),
                    status: *status,
                    kind: if source.redirect { REDIRECT } else { LINK },
                    links: source.links.into(),
                    nofollow: source.nofollow,
                })
            })
            .collect())
    }

    /// The pages that link or redirect to a URL, a page of them (redirects
    /// first, then in the order the crawl found the pages); and how many
    /// there are.
    pub fn inlinks(&self, crawl_id: i64, url_id: i64, offset: i64, limit: i64) -> Result<(i64, Vec<LinkRow>)> {
        let Some(mut row) =
            self.query_row("SELECT data FROM inlinks WHERE crawl_id = ?1 AND to_id = ?2", &params![crawl_id, url_id])?
        else {
            return Ok((0, Vec::new()));
        };
        let mut sources = unpack(&row.take_blob(0)?.unwrap_or_default())?;
        sources.sort_by_key(|source| (!source.redirect, source.from_id));
        let total = sources.len() as i64;
        let page: Vec<(i64, Source)> =
            sources.into_iter().skip(offset.max(0) as usize).take(limit.clamp(1, 1000) as usize).map(|source| (url_id, source)).collect();
        let targets = self.addresses(&[url_id])?;
        Ok((total, self.link_rows(&page, &targets)?))
    }

    /// Links whose target is broken, a page of them (by target address,
    /// then source); and how many there are.
    pub fn broken_links(&self, crawl_id: i64, offset: i64, limit: i64) -> Result<(i64, Vec<LinkRow>)> {
        let mut edges = Vec::new();
        let mut targets = HashMap::new();
        self.query(
            &format!(
                "SELECT i.to_id, t.url, t.status, i.data FROM inlinks i JOIN urls t ON t.id = i.to_id
                 WHERE i.crawl_id = ?1 AND {BROKEN} ORDER BY t.url"
            ),
            &params![crawl_id],
            |mut row| {
                let to_id: i64 = row.get(0)?;
                targets.insert(to_id, (row.get::<String>(1)?, row.get::<u16>(2)?));
                edges.extend(unpack(&row.take_blob(3)?.unwrap_or_default())?.into_iter().map(|source| (to_id, source)));
                Ok(())
            },
        )?;
        let total = edges.len() as i64;
        let page: Vec<(i64, Source)> = edges.into_iter().skip(offset.max(0) as usize).take(limit.clamp(1, 5000) as usize).collect();
        Ok((total, self.link_rows(&page, &targets)?))
    }

    /// The crawl's URLs among `urls`: address to (id, status).
    pub fn urls_by_address(&self, crawl_id: i64, urls: &[&str]) -> Result<HashMap<String, (i64, u16)>> {
        let mut found = HashMap::new();
        for chunk in urls.chunks(200) {
            let marks = (0..chunk.len()).map(|at| format!("?{}", at + 2)).collect::<Vec<_>>().join(", ");
            let mut values: Vec<Param> = vec![Param::Int(crawl_id)];
            values.extend(chunk.iter().map(|url| Param::from(*url)));
            self.query(&format!("SELECT url, id, status FROM urls WHERE crawl_id = ?1 AND url IN ({marks})"), &values, |row| {
                found.insert(row.get::<String>(0)?, (row.get(1)?, row.get(2)?));
                Ok(())
            })?;
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Write;
    use seo_page::HttpInfo;

    fn graph(target: &str) {
        let mut store = crate::tests::empty(target);
        let crawl = store.start_crawl("https://e.com/", "{}").unwrap();
        let response = |id: i64, path: &str, status: u16| Write::Response {
            id,
            http: HttpInfo { url: format!("https://e.com{path}"), status, ..Default::default() },
            internal: true,
            depth: Some(0),
        };
        store
            .apply(crawl, &[response(1, "/", 200), response(2, "/a", 200), response(3, "/gone", 404), response(4, "/old", 301)])
            .unwrap();
        assert!(!store.has_edges(crawl).unwrap());
        let source = |from_id, links, redirect, nofollow| Source { from_id, links, redirect, nofollow };
        let graph = || vec![
            (2, vec![source(4, 1, true, false), source(1, 2, false, false)]),
            (3, vec![source(2, 1, false, false), source(1, 1, false, true)]),
        ];
        store.save_inlinks(crawl, &mut graph()).unwrap();
        // Saving again replaces.
        store.save_inlinks(crawl, &mut graph()).unwrap();
        assert!(!store.has_edges(crawl).unwrap(), "the graph is whole with its scores");
        let scores = [(1, 0, Some(100)), (2, 2, Some(60)), (3, 2, Some(20)), (4, 0, None)];
        store.save_link_scores(crawl, &scores).unwrap();
        store.save_link_scores(crawl, &scores).unwrap();
        assert!(store.has_edges(crawl).unwrap());
        assert_eq!((store.link_score(2).unwrap(), store.link_score(4).unwrap()), (Some(60), None));
        assert_eq!(store.url_crawl(2).unwrap(), Some(crawl));
        let (total, rows) = store.inlinks(crawl, 2, 0, 10).unwrap();
        assert_eq!(total, 2);
        // The redirect first, then links by address.
        assert_eq!(rows.iter().map(|row| (row.from_url.as_str(), row.kind, row.links)).collect::<Vec<_>>(), [
            ("https://e.com/old", REDIRECT, 1),
            ("https://e.com/", LINK, 2)
        ]);
        let (total, broken) = store.broken_links(crawl, 0, 10).unwrap();
        assert_eq!(total, 2);
        assert_eq!(broken.iter().map(|row| (row.from_url.as_str(), row.status, row.nofollow)).collect::<Vec<_>>(), [
            ("https://e.com/", 404, true),
            ("https://e.com/a", 404, false)
        ]);
        let found = store.urls_by_address(crawl, &["https://e.com/a", "https://e.com/nope"]).unwrap();
        assert_eq!(found.get("https://e.com/a"), Some(&(2, 200)));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn packs_sources_both_ways() {
        let source = |from_id, links, redirect, nofollow| Source { from_id, links, redirect, nofollow };
        let sources = [source(3, 1, false, false), source(3, 1, true, false), source(200, 40, false, true), source(1 << 40, 2, false, false)];
        assert_eq!(unpack(&pack(&sources)).unwrap(), sources);
        assert!(unpack(&[0x80]).is_err());
    }

    #[test]
    fn the_graph_answers_inlinks_and_broken_links() {
        let path = std::env::temp_dir().join(format!("seo-store-graph-{}.sqlite", std::process::id()));
        graph(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_graph_on_servers() {
        let _one = crate::tests::SERVER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for target in crate::tests::servers() {
            graph(&target);
        }
    }
}
