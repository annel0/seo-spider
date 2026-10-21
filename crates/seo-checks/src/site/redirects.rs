//! Redirect chains and loops, followed through the pages the crawl visited.

use super::{Found, Index};
use crate::Issue;

/// A path longer than this is shortened in the detail.
const SHOWN_HOPS: usize = 5;

pub(super) fn check(index: &Index, out: &mut Found) {
    for (start, page) in index.own_pages() {
        if page.location.is_none() {
            continue;
        }
        let walk = follow(index, start);
        let redirecting = walk
            .path
            .iter()
            .filter(|&&position| index.pages[position].location.is_some())
            .count();

        if walk.returns_to == Some(start) {
            out.push((start, Issue::with_detail("redirect_loop", describe(index, &walk))));
        } else if redirecting >= 2 {
            // Two or more redirects in a row, including a path that runs into
            // a loop it is not itself part of.
            out.push((start, Issue::with_detail("redirect_chain", describe(index, &walk))));
        }
    }
}

/// The pages a redirect passes through, from `start` until it reaches a page
/// that does not redirect, a URL the crawl never visited, or a page it has
/// already been through.
struct Walk {
    path: Vec<usize>,
    /// The position of the page the path ran back into, if it did.
    returns_to: Option<usize>,
}

fn follow(index: &Index, start: usize) -> Walk {
    let mut path = vec![start];
    loop {
        let current = *path.last().expect("path starts with one page");
        let next = index.pages[current].location.as_deref().and_then(|target| index.position(target));
        let Some(next) = next else {
            return Walk { path, returns_to: None };
        };
        if path.contains(&next) {
            return Walk { path, returns_to: Some(next) };
        }
        path.push(next);
    }
}

/// `a → b → c`, ending at the final destination or back at the page the loop
/// closes on.
fn describe(index: &Index, walk: &Walk) -> String {
    let mut stops: Vec<&str> = walk.path.iter().map(|&position| index.pages[position].url.as_str()).collect();
    match walk.returns_to {
        Some(position) => stops.push(index.pages[position].url.as_str()),
        None => {
            let last = &index.pages[*walk.path.last().expect("path is not empty")];
            if let Some(destination) = last.location.as_deref() {
                stops.push(destination);
            }
        }
    }
    if stops.len() > SHOWN_HOPS {
        let last = stops[stops.len() - 1];
        stops.truncate(SHOWN_HOPS - 2);
        stops.push("…");
        stops.push(last);
    }
    stops.join(" → ")
}
