//! Near-duplicate content: pages whose main content shares most of its
//! five-word shingles with another page's, found from their MinHash
//! signatures as Screaming Frog finds them. Locality-sensitive hashing (16
//! bands of 8 values) picks the pairs worth comparing: a pair at 90%
//! similarity shares a band almost surely, one at 50% seldom. A pair at or
//! above the threshold is reported on both pages; exact duplicates are left
//! to `duplicate_content`.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use seo_page::{similarity, SIGNATURE};

use super::{Found, Index};
use crate::Issue;

const BANDS: usize = 16;
const ROWS: usize = SIGNATURE / BANDS;
/// Pages each page is compared with at most within one bucket: a template
/// shared by thousands of near-empty pages puts them all in one.
const BUCKET_LIMIT: usize = 200;

/// One band of a signature.
fn band(signature: &[u16], at: usize) -> &[u16] {
    &signature[at * ROWS..(at + 1) * ROWS]
}

/// The closest match of a page and how many pages pass the threshold.
struct Matches {
    closest: usize,
    similarity: f64,
    count: usize,
}

pub(super) fn check(index: &Index, threshold: f64, out: &mut Found) {
    let pages: Vec<(usize, &[u16])> = index
        .own_pages()
        .filter(|(_, page)| page.html && page.is_success() && page.is_self_canonical())
        .filter_map(|(position, page)| {
            page.minhash.as_deref().filter(|signature| signature.len() == SIGNATURE).map(|signature| (position, signature))
        })
        .collect();
    let mut matches: HashMap<usize, Matches> = HashMap::new();
    let mut record = |page: usize, other: usize, found: f64| {
        let entry = matches.entry(page).or_insert(Matches { closest: other, similarity: found, count: 0 });
        entry.count += 1;
        if found > entry.similarity {
            entry.closest = other;
            entry.similarity = found;
        }
    };
    for at in 0..BANDS {
        let mut buckets: HashMap<u64, Vec<u32>> = HashMap::new();
        for (member, (_, signature)) in pages.iter().enumerate() {
            let mut hasher = DefaultHasher::new();
            band(signature, at).hash(&mut hasher);
            buckets.entry(hasher.finish()).or_default().push(member as u32);
        }
        for members in buckets.values().filter(|members| members.len() > 1) {
            for (first, &a) in members.iter().enumerate() {
                for &b in members[first + 1..].iter().take(BUCKET_LIMIT) {
                    let ((page_a, signature_a), (page_b, signature_b)) = (pages[a as usize], pages[b as usize]);
                    // Compared already in the first band the two share.
                    if (0..at).any(|earlier| band(signature_a, earlier) == band(signature_b, earlier)) {
                        continue;
                    }
                    let text_a = index.pages[page_a].text_hash;
                    if text_a.is_some() && text_a == index.pages[page_b].text_hash {
                        continue;
                    }
                    let found = similarity(signature_a, signature_b);
                    if found >= threshold {
                        record(page_a, page_b, found);
                        record(page_b, page_a, found);
                    }
                }
            }
        }
    }
    let mut found: Vec<(usize, Matches)> = matches.into_iter().collect();
    found.sort_by_key(|(page, _)| *page);
    for (page, matches) in found {
        let more = match matches.count {
            1 => String::new(),
            count => format!(" (ещё {} стр.)", count - 1),
        };
        out.push((
            page,
            Issue::with_detail(
                "near_duplicate_content",
                format!(
                    "совпадает на {:.0}% с {}{more}",
                    (matches.similarity * 100.0).floor(),
                    index.pages[matches.closest].url
                ),
            ),
        ));
    }
}
