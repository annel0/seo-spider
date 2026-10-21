//! The store on a thread of its own. The dispatcher hands it writes over a
//! bounded channel and goes on; the thread applies whatever has queued up in
//! one transaction, so the slower the disk, the bigger the batches.

use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use seo_store::{Store, Write};
use tokio::sync::mpsc;

/// Most writes applied in one transaction.
const MAX_BATCH: usize = 4096;

pub struct Writer {
    sender: mpsc::Sender<Write>,
    thread: JoinHandle<Result<Store>>,
}

impl Writer {
    pub fn spawn(mut store: Store, crawl_id: i64) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel::<Write>(MAX_BATCH);
        let thread = std::thread::Builder::new().name("store-writer".into()).spawn(move || {
            let mut batch = Vec::with_capacity(MAX_BATCH);
            let (mut busy, mut batches, mut writes) = (std::time::Duration::ZERO, 0usize, 0usize);
            while let Some(first) = receiver.blocking_recv() {
                batch.push(first);
                while batch.len() < MAX_BATCH {
                    match receiver.try_recv() {
                        Ok(write) => batch.push(write),
                        Err(_) => break,
                    }
                }
                let started = std::time::Instant::now();
                store.apply(crawl_id, &batch)?;
                busy += started.elapsed();
                batches += 1;
                writes += batch.len();
                batch.clear();
            }
            tracing::info!(busy_s = busy.as_secs_f64(), batches, writes, "store writer");
            Ok(store)
        })?;
        Ok(Writer { sender, thread })
    }

    /// Queues a write, waiting while the channel is full.
    pub async fn send(&self, write: Write) -> Result<()> {
        self.sender.send(write).await.map_err(|_| anyhow!("the store writer stopped"))
    }

    /// Applies what is queued and hands the store back.
    pub fn finish(self) -> Result<Store> {
        drop(self.sender);
        self.thread.join().map_err(|_| anyhow!("the store writer panicked"))?
    }
}
