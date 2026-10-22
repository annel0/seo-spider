//! Plain-text summaries of a crawl, and of two crawls side by side.

use std::collections::HashMap;

use anyhow::Result;
use seo_store::params;
use seo_store::{Store, StoredUrl};

fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

pub fn report(store: &Store, crawl_id: i64) -> Result<()> {
    let urls = store.urls(crawl_id)?;
    let mut statuses: HashMap<String, usize> = HashMap::new();
    for url in &urls {
        let class = match url.http.status {
            0 if url.blocked_by_robots => "blocked".to_string(),
            0 => "no response".to_string(),
            status => format!("{}xx", status / 100),
        };
        *statuses.entry(format!("{} {class}", if url.internal { "internal" } else { "external" })).or_default() += 1;
    }
    let mut statuses: Vec<_> = statuses.into_iter().collect();
    statuses.sort();
    println!("URLs: {}", urls.len());
    for (class, count) in statuses {
        println!("  {class}: {count}");
    }
    let pages = urls.iter().filter(|url| url.raw.is_some()).count();
    println!("pages read: {pages}");

    type Row = (i64, i64, bool, bool, Option<String>, String, String);
    let rows: Vec<Row> = store
        .query_rows(
            "SELECT r.dcl_ms, r.total_ms, r.settled, r.load_fired, r.error, r.console_errors, r.resources
             FROM renders r JOIN urls u ON u.id = r.url_id WHERE u.crawl_id = ?1",
            &params![crawl_id],
        )?
        .iter()
        .map(|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))
        .collect::<Result<_>>()?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut dcl: Vec<i64> = rows.iter().map(|row| row.0).collect();
    let mut total: Vec<i64> = rows.iter().map(|row| row.1).collect();
    dcl.sort_unstable();
    total.sort_unstable();
    println!(
        "renders: {} (errors {}, settled {}, load fired {})",
        rows.len(),
        rows.iter().filter(|row| row.4.is_some()).count(),
        rows.iter().filter(|row| row.2).count(),
        rows.iter().filter(|row| row.3).count(),
    );
    println!(
        "  DOMContentLoaded ms: median {}, p90 {}, max {}",
        percentile(&dcl, 0.5),
        percentile(&dcl, 0.9),
        percentile(&dcl, 1.0)
    );
    println!(
        "  snapshot ms:         median {}, p90 {}, max {}",
        percentile(&total, 0.5),
        percentile(&total, 0.9),
        percentile(&total, 1.0)
    );

    let mut errors: HashMap<String, usize> = HashMap::new();
    let mut failed: HashMap<String, usize> = HashMap::new();
    for (_, _, _, _, _, console, resources) in &rows {
        let console: Vec<String> = serde_json::from_str(console).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        for error in console {
            let first = error.lines().next().unwrap_or("").chars().take(140).collect::<String>();
            if seen.insert(first.clone()) {
                *errors.entry(first).or_default() += 1;
            }
        }
        let resources: Vec<serde_json::Value> = serde_json::from_str(resources).unwrap_or_default();
        for resource in resources {
            let status = resource["status"].as_u64().unwrap_or(0);
            if status == 0 || status >= 400 {
                let url = resource["url"].as_str().unwrap_or("");
                let url = url.split('?').next().unwrap_or(url);
                let key = format!(
                    "{} {} {} {}",
                    resource["kind"].as_str().unwrap_or("?"),
                    status,
                    resource["error"].as_str().unwrap_or(""),
                    url.chars().take(100).collect::<String>()
                );
                *failed.entry(key).or_default() += 1;
            }
        }
    }
    let top = |map: HashMap<String, usize>| {
        let mut entries: Vec<_> = map.into_iter().collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        entries.truncate(10);
        entries
    };
    println!("console errors (pages):");
    for (error, count) in top(errors) {
        println!("  {count:5}  {error}");
    }
    println!("failed requests:");
    for (request, count) in top(failed) {
        println!("  {count:5}  {request}");
    }

    let (mut more, mut fewer) = (0, 0);
    for url in &urls {
        let (Some(raw), Some(rendered)) = (&url.raw, &url.rendered) else { continue };
        if rendered.links.len() > raw.links.len() || rendered.word_count as f64 > raw.word_count as f64 * 1.05 {
            more += 1;
        }
        if (rendered.links.len() as f64) < raw.links.len() as f64 * 0.95
            || (rendered.word_count as f64) < raw.word_count as f64 * 0.95
        {
            fewer += 1;
        }
    }
    println!("rendered vs raw: {more} pages gain links or words, {fewer} lose 5%+");
    Ok(())
}

/// Pages of two crawls whose rendered documents differ.
pub fn compare(a: &Store, a_id: i64, b: &Store, b_id: i64) -> Result<()> {
    let by_url = |urls: Vec<StoredUrl>| -> HashMap<String, StoredUrl> {
        urls.into_iter().map(|url| (url.http.url.clone(), url)).collect()
    };
    let left = by_url(a.urls(a_id)?);
    let right = by_url(b.urls(b_id)?);
    let mut common = 0;
    let mut differing = Vec::new();
    for (url, l) in &left {
        let Some(r) = right.get(url) else { continue };
        let (Some(lf), Some(rf)) = (l.facts(), r.facts()) else { continue };
        common += 1;
        let words = (lf.word_count as i64, rf.word_count as i64);
        let links = (lf.links.len() as i64, rf.links.len() as i64);
        let off = |(x, y): (i64, i64)| (x - y).abs() as f64 > (x.max(y) as f64 * 0.05).max(2.0);
        if off(words) || off(links) || lf.title() != rf.title() {
            differing.push((url.clone(), words, links, lf.title() != rf.title()));
        }
    }
    println!(
        "{} pages in both ({} only in the first, {} only in the second); {} differ by 5%+ in words or links, or in title",
        common,
        left.keys().filter(|url| !right.contains_key(*url)).count(),
        right.keys().filter(|url| !left.contains_key(*url)).count(),
        differing.len()
    );
    differing.sort();
    for (url, words, links, title) in differing.iter().take(30) {
        println!(
            "  words {:>5} -> {:<5} links {:>4} -> {:<4}{}  {url}",
            words.0,
            words.1,
            links.0,
            links.1,
            if *title { " title" } else { "" }
        );
    }
    Ok(())
}
