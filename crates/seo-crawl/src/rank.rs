//! Internal PageRank: how much of the site's link weight each internal URL
//! gets, from the crawl's link graph, as a link score of 0 to 100 (the most
//! linked page 100, on a log scale, as Screaming Frog's Link Score). Links
//! between internal URLs count, once per page and target; nofollow links
//! pass nothing, a redirect passes it all on. A URL nothing links to
//! scores 0.

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use seo_store::Source;

const DAMPING: f64 = 0.85;
const ROUNDS: usize = 40;
/// Close enough: the ranks moved less than this in all, over a round.
const SETTLED: f64 = 1e-7;

/// Per URL of the graph (in its order): pages linking to it, and its link
/// score when it is internal.
pub(crate) fn link_scores(graph: &[(i64, Vec<Source>)], internal: &[bool]) -> Vec<(i64, i64, Option<i64>)> {
    let at: FxHashMap<i64, usize> = graph.iter().enumerate().map(|(index, (id, _))| (*id, index)).collect();
    // Followed internal links, by target: where from.
    let mut outgoing = vec![0u32; graph.len()];
    let incoming: Vec<Vec<u32>> = graph
        .iter()
        .enumerate()
        .map(|(to, (_, sources))| {
            if !internal[to] {
                return Vec::new();
            }
            sources
                .iter()
                .filter(|source| !source.nofollow)
                .filter_map(|source| at.get(&source.from_id).copied())
                .filter(|&from| internal[from])
                .map(|from| from as u32)
                .collect()
        })
        .collect();
    for from in incoming.iter().flatten() {
        outgoing[*from as usize] += 1;
    }
    let pages = internal.iter().filter(|internal| **internal).count();
    let mut rank: Vec<f64> = internal.iter().map(|&internal| if internal && pages > 0 { 1.0 / pages as f64 } else { 0.0 }).collect();
    for _ in 0..ROUNDS {
        // What pages without followed links hold goes to every page.
        let dangling: f64 =
            (0..graph.len()).filter(|&page| internal[page] && outgoing[page] == 0).map(|page| rank[page]).sum::<f64>();
        let base = ((1.0 - DAMPING) + DAMPING * dangling) / pages.max(1) as f64;
        let next: Vec<f64> = incoming
            .par_iter()
            .enumerate()
            .map(|(to, from)| {
                if !internal[to] {
                    return 0.0;
                }
                base + DAMPING * from.iter().map(|&from| rank[from as usize] / f64::from(outgoing[from as usize])).sum::<f64>()
            })
            .collect();
        let moved: f64 = next.iter().zip(&rank).map(|(now, then)| (now - then).abs()).sum();
        rank = next;
        if moved < SETTLED {
            break;
        }
    }
    // As many times an even share, on a log scale from the least linked
    // page to the most.
    let share = |rank: f64| (rank * pages as f64).max(0.0).ln_1p();
    let shares = || (0..graph.len()).filter(|&page| internal[page]).map(|page| share(rank[page]));
    let (low, top) = (shares().fold(f64::INFINITY, f64::min), shares().fold(0.0, f64::max));
    let score = |rank: f64| if top - low > 1e-12 { (100.0 * (share(rank) - low) / (top - low)).round() as i64 } else { 100 };
    graph
        .iter()
        .enumerate()
        .map(|(index, (id, sources))| (*id, sources.len() as i64, internal[index].then(|| score(rank[index]))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(from_id: i64) -> Source {
        Source { from_id, links: 1, redirect: false, nofollow: false }
    }

    #[test]
    fn the_most_linked_page_scores_most() {
        // 1 is the home page every page links to; 4 only 1 links to; 5 is
        // external; 6 is linked nofollow only; 2 redirects to 3.
        let graph = vec![
            (1, vec![link(2), link(3), link(4), link(6)]),
            (2, vec![link(1)]),
            (3, vec![link(1), Source { redirect: true, ..link(2) }]),
            (4, vec![link(1)]),
            (5, vec![link(1), link(3)]),
            (6, vec![Source { nofollow: true, ..link(1) }]),
        ];
        let internal = [true, true, true, true, false, true];
        let scores = link_scores(&graph, &internal);
        let score = |id: i64| scores.iter().find(|row| row.0 == id).unwrap().2;
        assert_eq!((score(1), score(6)), (Some(100), Some(0)));
        assert_eq!(score(5), None);
        // 3 gets 2's weight through the redirect as well as a link from 1.
        assert!(score(3) > score(4) && score(4) > score(6), "{scores:?}");
        assert_eq!(scores.iter().map(|row| row.1).collect::<Vec<_>>(), [4, 1, 2, 1, 2, 1]);
        // Nothing internal: no scores.
        assert!(link_scores(&graph, &[false; 6]).iter().all(|row| row.2.is_none()));
    }
}
