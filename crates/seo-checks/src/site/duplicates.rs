//! Pages that repeat each other: title, meta description, H1 and the text
//! itself. Only pages that stand for themselves compete, so a page that points
//! its canonical elsewhere may share anything with its target.

use std::collections::HashMap;
use std::hash::Hash;

use super::{Found, Index};
use crate::{Issue, SitePage};

pub(super) fn check(index: &Index, out: &mut Found) {
    report(index, out, "title_duplicate", "таким же title", |page| text_in_language(&page.title, page));
    report(index, out, "description_duplicate", "таким же описанием", |page| {
        text_in_language(&page.description, page)
    });
    report(index, out, "h1_duplicate", "таким же H1", |page| text_in_language(&page.h1, page));
    report(index, out, "duplicate_content", "таким же текстом", |page| page.text_hash);
}

/// The same words in different languages are not a duplicate, so the language
/// of the page is part of the key. Case does not matter.
fn text_in_language(text: &Option<String>, page: &SitePage) -> Option<(String, String)> {
    text.as_ref().map(|text| (text.to_lowercase(), page.lang.as_deref().unwrap_or("").to_lowercase()))
}

/// Groups the pages that compete by `key` and reports every member of a group
/// with more than one page.
fn report<K: Hash + Eq>(
    index: &Index,
    out: &mut Found,
    code: &'static str,
    what: &str,
    key: impl Fn(&SitePage) -> Option<K>,
) {
    let mut groups: HashMap<K, Vec<usize>> = HashMap::new();
    for (position, page) in index.own_pages() {
        if !(page.html && page.is_success() && page.is_self_canonical()) {
            continue;
        }
        if let Some(key) = key(page) {
            groups.entry(key).or_default().push(position);
        }
    }

    for members in groups.values().filter(|members| members.len() > 1) {
        for &position in members {
            let other = members.iter().copied().find(|&member| member != position).expect("group has two pages");
            out.push((
                position,
                Issue::with_detail(
                    code,
                    format!("ещё {} стр. с {what}, например {}", members.len() - 1, index.pages[other].url),
                ),
            ));
        }
    }
}
