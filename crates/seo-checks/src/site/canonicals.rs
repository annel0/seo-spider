//! Where canonical tags and hreflang annotations lead. The problem is reported
//! on the page that carries the tag, since that is where it gets fixed; the
//! detail names the targets.

use std::collections::HashMap;

use super::{push_unique, Found, Index};
use crate::{urls, Issue, SitePage};

pub(super) fn check(index: &Index, out: &mut Found) {
    for (position, page) in index.own_pages() {
        canonical_target(index, position, page, out);
        hreflang_targets(index, position, page, out);
    }
    several_languages(index, out);
}

fn canonical_target(index: &Index, position: usize, page: &SitePage, out: &mut Found) {
    let Some(canonical) = page.canonical.as_deref() else { return };
    if !page.is_success() || page.is_self_canonical() {
        return;
    }
    // A canonical the crawl never reached cannot be judged.
    let Some(target) = index.get(canonical) else { return };

    if target.is_redirect() {
        out.push((position, Issue::with_detail("canonical_to_redirect", canonical)));
    } else if target.is_error() {
        let detail = format!("{canonical} (статус {})", target.status);
        out.push((position, Issue::with_detail("canonical_to_error", detail)));
    } else if target.is_success() {
        if let Some(beyond) = target.canonical.as_deref().filter(|_| target.is_non_canonical()) {
            let detail = format!("{canonical} сам указывает на {beyond}");
            out.push((position, Issue::with_detail("canonical_to_non_canonical", detail)));
        }
        if target.is_closed_to_index() {
            out.push((position, Issue::with_detail("canonical_to_non_indexable", canonical)));
        }
    }
}

/// Problems with the pages one page's hreflang annotations point to.
fn hreflang_targets(index: &Index, position: usize, page: &SitePage, out: &mut Found) {
    if !page.is_success() || page.hreflangs.is_empty() {
        return;
    }
    let mut problems = HreflangProblems::default();
    for item in &page.hreflangs {
        if urls::same_url(&item.href, &page.url) {
            continue;
        }
        // Targets the crawl never reached cannot be judged.
        let Some(target) = index.get(&item.href) else { continue };

        if target.is_redirect() {
            push_unique(&mut problems.redirect, &item.href);
        } else if target.is_error() {
            push_unique(&mut problems.error, &item.href);
        } else if target.is_success() {
            if target.is_non_canonical() {
                push_unique(&mut problems.non_canonical, &item.href);
            }
            if target.is_closed_to_index() {
                push_unique(&mut problems.closed, &item.href);
            }
            // The other language must link back. `x-default` is a fallback and
            // does not have to, and a page that is not canonical is not the
            // one that announces the language versions.
            let answers = target.hreflangs.iter().any(|back| urls::same_url(&back.href, &page.url));
            if !item.lang.eq_ignore_ascii_case("x-default") && page.is_self_canonical() && !answers {
                push_unique(&mut problems.no_return, &item.href);
            }
        }
    }

    for (code, targets) in [
        ("hreflang_no_return_link", &problems.no_return),
        ("hreflang_to_non_canonical", &problems.non_canonical),
        ("hreflang_to_noindex", &problems.closed),
        ("hreflang_to_redirect", &problems.redirect),
        ("hreflang_to_error", &problems.error),
    ] {
        if !targets.is_empty() {
            out.push((position, Issue::with_detail(code, urls::examples(targets, 3))));
        }
    }
}

#[derive(Default)]
struct HreflangProblems {
    no_return: Vec<String>,
    non_canonical: Vec<String>,
    closed: Vec<String>,
    redirect: Vec<String>,
    error: Vec<String>,
}

/// A URL named by hreflang annotations with more than one language (other than
/// `x-default`). Reported on the URL that is named inconsistently.
fn several_languages(index: &Index, out: &mut Found) {
    let mut named: HashMap<usize, Vec<String>> = HashMap::new();
    for (_, page) in index.own_pages() {
        for item in &page.hreflangs {
            if item.lang.eq_ignore_ascii_case("x-default") {
                continue;
            }
            let Some(target) = index.position(&item.href) else { continue };
            let langs = named.entry(target).or_default();
            let lang = item.lang.to_ascii_lowercase();
            if !langs.contains(&lang) {
                langs.push(lang);
            }
        }
    }
    for (target, mut langs) in named {
        if langs.len() > 1 && !index.pages[target].external {
            langs.sort();
            out.push((target, Issue::with_detail("hreflang_multiple_langs", langs.join(", "))));
        }
    }
}
