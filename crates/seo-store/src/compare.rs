//! Two crawls of a site side by side: the URLs that appeared and went, the
//! ones whose status, title, description, H1, canonical, robots or amount
//! of text changed, and the issues that are new or fixed. Read from the
//! columns the store keeps next to the facts, no facts decoded: two crawls
//! of 100,000 URLs compare in four seconds.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;

use crate::{params, Store};

/// What the comparison reads of one URL.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    id: i64,
    status: u16,
    internal: bool,
    title: Option<String>,
    description: Option<String>,
    h1: Option<String>,
    canonical: Option<String>,
    robots: Option<String>,
    words: Option<i64>,
}

/// A URL in one of the crawls.
#[derive(Debug, Clone, Serialize)]
pub struct DiffUrl {
    pub url: String,
    pub id: i64,
    pub status: u16,
    pub internal: bool,
}

/// One field of one URL that changed.
#[derive(Debug, Clone, Serialize)]
pub struct DiffChange {
    pub url: String,
    /// The URL in the newer crawl.
    pub id: i64,
    /// status, title, description, h1, canonical, robots or words.
    pub field: &'static str,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// An issue of one URL, new in the newer crawl or gone from it.
#[derive(Debug, Clone, Serialize)]
pub struct DiffIssue {
    pub url: String,
    /// The URL in the crawl that has the issue.
    pub id: i64,
    pub code: String,
}

/// How many URLs had an issue before and after, how many got it and how
/// many lost it.
#[derive(Debug, Clone, Serialize)]
pub struct IssueShift {
    pub code: String,
    pub before: i64,
    pub after: i64,
    pub appeared: i64,
    pub fixed: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrawlDiff {
    pub old: i64,
    pub new: i64,
    pub added: Vec<DiffUrl>,
    pub removed: Vec<DiffUrl>,
    pub changed: Vec<DiffChange>,
    pub new_issues: Vec<DiffIssue>,
    pub fixed_issues: Vec<DiffIssue>,
    /// The issue codes with URLs that got or lost them, the largest change
    /// in count first.
    pub shifts: Vec<IssueShift>,
}

/// Every field `compare` can compare besides the status.
pub const FIELDS: [&str; 6] = ["title", "description", "h1", "canonical", "robots", "words"];

/// Text changes this much (a fifth, and 50 words) before it counts.
fn words_changed(before: i64, after: i64) -> bool {
    let difference = (after - before).abs();
    difference >= 50 && difference * 5 >= before.max(after)
}

impl Store {
    fn seen(&self, crawl_id: i64) -> Result<HashMap<String, Seen>> {
        let effective = |column: &str| format!("CASE WHEN fr.url_id IS NULL THEN fw.{column} ELSE fr.{column} END");
        let sql = format!(
            "SELECT u.url, u.id, u.status, u.internal, {}, {}, {}, {}, {}, {}
             FROM urls u
             LEFT JOIN facts fw ON fw.url_id = u.id AND fw.source = 'raw'
             LEFT JOIN facts fr ON fr.url_id = u.id AND fr.source = 'rendered'
             WHERE u.crawl_id = ?1",
            effective("title"),
            effective("description"),
            effective("h1"),
            effective("canonical"),
            effective("robots"),
            effective("word_count"),
        );
        let mut seen = HashMap::new();
        self.query(&sql, &params![crawl_id], |mut row| {
            let url = row.take_string(0)?;
            seen.insert(
                url,
                Seen {
                    id: row.get(1)?,
                    status: row.get(2)?,
                    internal: row.get(3)?,
                    title: row.take_text(4)?,
                    description: row.take_text(5)?,
                    h1: row.take_text(6)?,
                    canonical: row.take_text(7)?,
                    robots: row.take_text(8)?,
                    words: row.get(9)?,
                },
            );
            Ok(())
        })?;
        Ok(seen)
    }

    fn issue_pairs(&self, crawl_id: i64) -> Result<HashSet<(String, String)>> {
        let mut pairs = HashSet::new();
        self.query(
            "SELECT u.url, i.code FROM issues i JOIN urls u ON u.id = i.url_id WHERE i.crawl_id = ?1",
            &params![crawl_id],
            |mut row| {
                pairs.insert((row.take_string(0)?, row.take_string(1)?));
                Ok(())
            },
        )?;
        Ok(pairs)
    }

    /// What changed from crawl `old` to crawl `new`: the status, and of
    /// `fields` (title, description, h1, canonical, robots, words) those
    /// both crawls read.
    pub fn compare(&self, old: i64, new: i64, fields: &[&str]) -> Result<CrawlDiff> {
        let (before, after) = (self.seen(old)?, self.seen(new)?);
        let url = |url: &str, seen: &Seen| DiffUrl { url: url.to_string(), id: seen.id, status: seen.status, internal: seen.internal };
        let mut added: Vec<DiffUrl> =
            after.iter().filter(|(address, _)| !before.contains_key(*address)).map(|(address, seen)| url(address, seen)).collect();
        let mut removed: Vec<DiffUrl> =
            before.iter().filter(|(address, _)| !after.contains_key(*address)).map(|(address, seen)| url(address, seen)).collect();
        let mut changed = Vec::new();
        for (address, now) in &after {
            let Some(then) = before.get(address) else { continue };
            let mut change = |field: &'static str, before: Option<String>, after: Option<String>| {
                if before != after && (field == "status" || fields.contains(&field)) {
                    changed.push(DiffChange { url: address.clone(), id: now.id, field, before, after });
                }
            };
            change("status", Some(then.status.to_string()), Some(now.status.to_string()));
            if !now.internal {
                continue;
            }
            change("title", then.title.clone(), now.title.clone());
            change("description", then.description.clone(), now.description.clone());
            change("h1", then.h1.clone(), now.h1.clone());
            change("canonical", then.canonical.clone(), now.canonical.clone());
            change("robots", then.robots.clone(), now.robots.clone());
            if let (Some(was), Some(is)) = (then.words, now.words) {
                if fields.contains(&"words") && words_changed(was, is) {
                    change("words", Some(was.to_string()), Some(is.to_string()));
                }
            }
        }
        let (issues_before, issues_after) = (self.issue_pairs(old)?, self.issue_pairs(new)?);
        let issue = |(address, code): &(String, String), crawl: &HashMap<String, Seen>| DiffIssue {
            url: address.clone(),
            id: crawl.get(address).map_or(0, |seen| seen.id),
            code: code.clone(),
        };
        let mut new_issues: Vec<DiffIssue> =
            issues_after.difference(&issues_before).map(|pair| issue(pair, &after)).collect();
        let mut fixed_issues: Vec<DiffIssue> =
            issues_before.difference(&issues_after).map(|pair| issue(pair, &before)).collect();
        let mut counts: BTreeMap<&str, IssueShift> = BTreeMap::new();
        fn count<'a, 'b>(counts: &'b mut BTreeMap<&'a str, IssueShift>, code: &'a str) -> &'b mut IssueShift {
            counts.entry(code).or_insert_with(|| IssueShift { code: code.to_string(), before: 0, after: 0, appeared: 0, fixed: 0 })
        }
        for (_, code) in &issues_before {
            count(&mut counts, code).before += 1;
        }
        for (_, code) in &issues_after {
            count(&mut counts, code).after += 1;
        }
        for issue in &new_issues {
            count(&mut counts, &issue.code).appeared += 1;
        }
        for issue in &fixed_issues {
            count(&mut counts, &issue.code).fixed += 1;
        }
        let mut shifts: Vec<IssueShift> = counts.into_values().filter(|shift| shift.appeared + shift.fixed > 0).collect();
        shifts.sort_by_key(|shift| std::cmp::Reverse(((shift.after - shift.before).abs(), shift.appeared + shift.fixed)));
        added.sort_by(|a, b| a.url.cmp(&b.url));
        removed.sort_by(|a, b| a.url.cmp(&b.url));
        changed.sort_by(|a, b| (a.field, &a.url).cmp(&(b.field, &b.url)));
        new_issues.sort_by(|a, b| (&a.code, &a.url).cmp(&(&b.code, &b.url)));
        fixed_issues.sort_by(|a, b| (&a.code, &a.url).cmp(&(&b.code, &b.url)));
        Ok(CrawlDiff { old, new, added, removed, changed, new_issues, fixed_issues, shifts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Write;
    use seo_page::{HttpInfo, Source};

    fn crawl(store: &mut Store, first_id: i64, pages: &[(&str, u16, &str)], issues: &[(usize, &'static str)]) -> i64 {
        let crawl = store.start_crawl("https://e.com/", "{}").unwrap();
        let mut writes = Vec::new();
        for (at, (path, status, title)) in pages.iter().enumerate() {
            let id = first_id + at as i64;
            let url = format!("https://e.com{path}");
            writes.push(Write::Response {
                id,
                http: HttpInfo { url: url.clone(), status: *status, content_type: Some("text/html".into()), ..Default::default() },
                internal: true,
                depth: Some(0),
            });
            let facts = seo_page::extract(&format!("<title>{title}</title>"), &url::Url::parse(&url).unwrap(), Source::Raw);
            let data = crate::encode_facts(&facts, 0).unwrap();
            writes.push(Write::Facts { url_id: id, facts: Box::new(facts), data, links: false });
        }
        for &(at, code) in issues {
            writes.push(Write::Issues { url_id: first_id + at as i64, issues: vec![(code, None)] });
        }
        store.apply(crawl, &writes).unwrap();
        crawl
    }

    fn compares(target: &str) {
        let mut store = crate::tests::empty(target);
        let old = crawl(&mut store, 1, &[("/", 200, "Home"), ("/a", 200, "A"), ("/gone", 200, "Gone")], &[(1, "title_too_short"), (2, "h1_missing")]);
        let new = crawl(&mut store, 10, &[("/", 200, "Home"), ("/a", 404, "A page"), ("/new", 200, "New")], &[(1, "client_error_4xx"), (1, "title_too_short")]);
        let diff = store.compare(old, new, &FIELDS).unwrap();
        assert_eq!(diff.added.iter().map(|url| url.url.as_str()).collect::<Vec<_>>(), ["https://e.com/new"]);
        assert_eq!(diff.removed.iter().map(|url| url.url.as_str()).collect::<Vec<_>>(), ["https://e.com/gone"]);
        let changed: Vec<(&str, Option<&str>, Option<&str>)> =
            diff.changed.iter().map(|change| (change.field, change.before.as_deref(), change.after.as_deref())).collect();
        assert_eq!(changed, [("status", Some("200"), Some("404")), ("title", Some("A"), Some("A page"))]);
        assert_eq!(diff.changed[0].id, 11);
        assert_eq!(diff.new_issues.iter().map(|issue| issue.code.as_str()).collect::<Vec<_>>(), ["client_error_4xx"]);
        assert_eq!(diff.fixed_issues.iter().map(|issue| (issue.code.as_str(), issue.id)).collect::<Vec<_>>(), [("h1_missing", 3)]);
        assert_eq!(diff.shifts.len(), 2);
        assert_eq!(store.compare(old, new, &[]).unwrap().changed.len(), 1);
        assert!(words_changed(100, 200) && !words_changed(1000, 1040) && !words_changed(10, 40));
    }

    #[test]
    fn two_crawls_compare() {
        let path = std::env::temp_dir().join(format!("seo-store-compare-{}.sqlite", std::process::id()));
        compares(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn two_crawls_compare_on_servers() {
        let _one = crate::tests::SERVER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for target in crate::tests::servers() {
            compares(&target);
        }
    }
}
