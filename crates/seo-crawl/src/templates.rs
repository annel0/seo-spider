//! The site's template, found while crawling: every page's text blocks are
//! counted by hash (`seo_page::block_hash`), and the blocks on many pages —
//! menus, header, footer, a section's sidebar — are kept with the crawl,
//! so a page's text can be shown without them.

use rustc_hash::FxHashMap;

/// Distinct blocks counted at most (about 30 MB). Past it the rarest
/// counts are dropped, and a block that comes back starts again; a template
/// block, on many pages, is never among them.
const COUNTED: usize = 1 << 20;

#[derive(Default)]
pub(crate) struct Templates {
    counts: FxHashMap<u64, u32>,
    /// Pages counted.
    pages: u32,
    /// Counts this low were dropped.
    dropped: u32,
}

impl Templates {
    /// Counting goes on from a crawl's saved template, `pages` pages in.
    pub(crate) fn resume(saved: Vec<(u64, u32)>, pages: u32) -> Self {
        Templates { counts: saved.into_iter().collect(), pages, dropped: 0 }
    }

    /// One page's distinct blocks.
    pub(crate) fn page(&mut self, blocks: &[u64]) {
        if blocks.is_empty() {
            return;
        }
        self.pages += 1;
        for block in blocks {
            *self.counts.entry(*block).or_default() += 1;
        }
        while self.counts.len() > COUNTED {
            self.dropped += 1;
            let dropped = self.dropped;
            self.counts.retain(|_, count| *count > dropped);
        }
    }

    /// The template: the blocks on enough of the pages, with how many.
    pub(crate) fn blocks(&self) -> Vec<(u64, u32)> {
        let least = least_pages(self.pages);
        self.counts.iter().filter(|(_, pages)| **pages >= least).map(|(block, pages)| (*block, *pages)).collect()
    }
}

/// Pages a block is on to be the template: a twentieth of them, between
/// 10 and 200. Text a page shares with a few listings (a post on the home
/// page, its category and tags) stays the page's own; a small site's
/// template is what half its pages have.
fn least_pages(pages: u32) -> u32 {
    let floor = 10.min(pages.div_ceil(2)).max(2);
    (pages / 20).clamp(floor, 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_blocks_on_many_pages() {
        assert_eq!([1, 4, 30, 1000, 100_000].map(least_pages), [2, 2, 10, 50, 200]);
        let mut templates = Templates::default();
        for page in 0..40u64 {
            // A menu on every page, a sidebar on every fourth, and text of its own.
            let mut blocks = vec![1, 1000 + page];
            if page % 4 == 0 {
                blocks.push(2);
            }
            templates.page(&blocks);
        }
        let mut found = templates.blocks();
        found.sort();
        assert_eq!(found, [(1, 40), (2, 10)]);
        let mut resumed = Templates::resume(found, 40);
        resumed.page(&[1, 7]);
        assert!(resumed.blocks().contains(&(1, 41)));
    }

    #[test]
    fn drops_rare_blocks_past_the_limit() {
        let mut templates = Templates::default();
        for page in 0..(COUNTED as u64 / 100 + 10) {
            let blocks: Vec<u64> = std::iter::once(1).chain((0..100).map(|at| 10 + page * 100 + at)).collect();
            templates.page(&blocks);
        }
        assert!(templates.counts.len() <= COUNTED);
        assert!(templates.blocks().contains(&(1, templates.pages)));
    }
}
