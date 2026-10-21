//! Links between the crawled pages: links to redirects and broken pages,
//! nofollow links to pages that should be indexed, mixed incoming links and
//! pages nothing links to.

use super::{push_unique, Found, Index};
use crate::{urls, Issue};

pub(super) fn check(index: &Index, out: &mut Found) {
    outgoing(index, out);
    incoming(index, out);
}

/// What a page links to, judged by the status of the target. Internal and
/// external links are reported separately.
fn outgoing(index: &Index, out: &mut Found) {
    for (position, page) in index.own_pages() {
        if !page.is_success() {
            continue;
        }
        let mut internal_redirect = Vec::new();
        let mut internal_error = Vec::new();
        let mut external_redirect = Vec::new();
        let mut external_error = Vec::new();
        let mut nofollow_to_indexable = Vec::new();

        for link in &page.links {
            let Some(target_position) = index.position(&link.target) else { continue };
            if target_position == position {
                continue;
            }
            let target = &index.pages[target_position];
            let (redirects, errors) = if link.internal {
                (&mut internal_redirect, &mut internal_error)
            } else {
                (&mut external_redirect, &mut external_error)
            };
            if target.is_redirect() {
                push_unique(redirects, &link.target);
            } else if target.is_error() {
                push_unique(errors, &link.target);
            } else if link.internal && link.nofollow && target.is_indexable() {
                push_unique(&mut nofollow_to_indexable, &link.target);
            }
        }

        for (code, targets) in [
            ("internal_link_to_redirect", &internal_redirect),
            ("internal_link_to_error", &internal_error),
            ("external_link_redirect", &external_redirect),
            ("external_link_broken", &external_error),
            ("nofollow_to_indexable", &nofollow_to_indexable),
        ] {
            if !targets.is_empty() {
                let detail = format!("{}: {}", targets.len(), urls::examples(targets, 3));
                out.push((position, Issue::with_detail(code, detail)));
            }
        }
    }
}

/// Incoming internal links counted per target, split by whether they pass
/// weight. A redirect to a page also counts as a way in.
fn incoming(index: &Index, out: &mut Found) {
    let mut followed = vec![0usize; index.pages.len()];
    let mut nofollowed = vec![0usize; index.pages.len()];
    let mut redirected_to = vec![false; index.pages.len()];

    for (position, page) in index.own_pages() {
        for link in page.links.iter().filter(|link| link.internal) {
            match index.position(&link.target) {
                Some(target) if target != position => {
                    if link.nofollow {
                        nofollowed[target] += 1;
                    } else {
                        followed[target] += 1;
                    }
                }
                _ => {}
            }
        }
        if let Some(target) = page.location.as_deref().and_then(|location| index.position(location)) {
            if target != position {
                redirected_to[target] = true;
            }
        }
    }

    for (position, page) in index.own_pages() {
        if !page.is_success() {
            continue;
        }
        if followed[position] > 0 && nofollowed[position] > 0 {
            let detail = format!("{} без nofollow, {} с nofollow", followed[position], nofollowed[position]);
            out.push((position, Issue::with_detail("incoming_follow_and_nofollow", detail)));
        }
        // A page the sitemap names but no page links to.
        let linked = followed[position] + nofollowed[position] > 0 || redirected_to[position];
        if page.in_sitemap && page.html && !linked {
            out.push((position, Issue::new("orphan_page")));
        }
    }
}
