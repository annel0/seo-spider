//! A crawl's site template: the text blocks it met on many of its pages
//! (menus, header, footer), by `seo_page::block_hash`, with how many pages
//! each was on.

use std::collections::HashMap;

use anyhow::Result;

use crate::schema::TEMPLATE_BLOCKS;
use crate::{params, Param, Store};

impl Store {
    /// Replaces the crawl's template.
    pub fn save_template(&mut self, crawl_id: i64, blocks: &[(u64, u32)]) -> Result<()> {
        self.transaction(|db| {
            db.execute("DELETE FROM template_blocks WHERE crawl_id = ?1", &params![crawl_id])?;
            for slice in blocks.chunks(16_384) {
                // The hash's bits as a signed integer, as the databases keep it.
                let rows: Vec<Vec<Param>> =
                    slice.iter().map(|(hash, pages)| params![crawl_id, *hash as i64, *pages].to_vec()).collect();
                db.insert_rows(&TEMPLATE_BLOCKS, &rows)?;
            }
            Ok(())
        })
    }

    /// The crawl's template: pages per block hash. Empty for a crawl made
    /// before templates were kept, or that read no text.
    pub fn template(&self, crawl_id: i64) -> Result<Vec<(u64, u32)>> {
        let mut blocks = Vec::new();
        self.query("SELECT hash, pages FROM template_blocks WHERE crawl_id = ?1", &params![crawl_id], |row| {
            blocks.push((row.get::<i64>(0)? as u64, row.get(1)?));
            Ok(())
        })?;
        Ok(blocks)
    }

    /// `template` as a map.
    pub fn template_map(&self, crawl_id: i64) -> Result<HashMap<u64, u32>> {
        Ok(self.template(crawl_id)?.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    fn keeps(target: &str) {
        let mut store = crate::tests::empty(target);
        let crawl = store.start_crawl("https://e.com/", "{}").unwrap();
        let blocks = [(u64::MAX - 1, 40), (7, 12)];
        store.save_template(crawl, &blocks).unwrap();
        store.save_template(crawl, &blocks).unwrap();
        let mut read = store.template(crawl).unwrap();
        read.sort();
        assert_eq!(read, [(7, 12), (u64::MAX - 1, 40)]);
        assert!(store.template(crawl + 1).unwrap().is_empty());
    }

    #[test]
    fn keeps_the_template() {
        let path = std::env::temp_dir().join(format!("seo-store-template-{}.sqlite", std::process::id()));
        keeps(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn keeps_the_template_on_servers() {
        let _one = crate::tests::SERVER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for target in crate::tests::servers() {
            keeps(&target);
        }
    }
}
