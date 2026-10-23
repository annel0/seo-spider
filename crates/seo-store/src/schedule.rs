//! Crawls on a schedule: what to crawl (the request the UI sends to start
//! one, as JSON), how often, when next, and where to report it.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{params, Row, Store};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    /// 0 for one not saved yet.
    #[serde(default)]
    pub id: i64,
    /// The crawl to start, as `POST /api/crawls` takes it.
    pub request: serde_json::Value,
    /// Hours from one run to the next.
    pub every_hours: u32,
    /// When it runs next, in Unix seconds.
    pub next_run: i64,
    /// The Telegram chat its reports go to.
    #[serde(default)]
    pub telegram_chat: Option<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// The crawl it started last.
    #[serde(default)]
    pub last_crawl: Option<i64>,
}

fn enabled() -> bool {
    true
}

const COLUMNS: &str = "id, request, every_hours, next_run, telegram_chat, enabled, last_crawl";

fn schedule(row: &mut Row) -> Result<Schedule> {
    Ok(Schedule {
        id: row.get(0)?,
        request: serde_json::from_str(&row.take_string(1)?)?,
        every_hours: row.get(2)?,
        next_run: row.get(3)?,
        telegram_chat: row.take_text(4)?,
        enabled: row.get(5)?,
        last_crawl: row.get(6)?,
    })
}

impl Store {
    pub fn schedules(&self) -> Result<Vec<Schedule>> {
        let mut schedules = Vec::new();
        self.query(&format!("SELECT {COLUMNS} FROM schedules ORDER BY id"), &[], |mut row| {
            schedules.push(schedule(&mut row)?);
            Ok(())
        })?;
        Ok(schedules)
    }

    pub fn schedule(&self, id: i64) -> Result<Option<Schedule>> {
        match self.query_row(&format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1"), &params![id])? {
            Some(mut row) => Ok(Some(schedule(&mut row)?)),
            None => Ok(None),
        }
    }

    /// Adds the schedule (id 0) or replaces the one with its id; its id.
    pub fn save_schedule(&self, schedule: &Schedule) -> Result<i64> {
        let request = serde_json::to_string(&schedule.request)?;
        let chat = schedule.telegram_chat.as_deref().map(str::trim).filter(|chat| !chat.is_empty());
        if schedule.id == 0 {
            return self.db.borrow_mut().insert_returning_id(
                "INSERT INTO schedules (request, every_hours, next_run, telegram_chat, enabled, last_crawl) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                &params![&request, schedule.every_hours, schedule.next_run, chat, schedule.enabled, schedule.last_crawl],
            );
        }
        self.execute(
            "UPDATE schedules SET request = ?2, every_hours = ?3, next_run = ?4, telegram_chat = ?5, enabled = ?6, \
             last_crawl = ?7 WHERE id = ?1",
            &params![schedule.id, &request, schedule.every_hours, schedule.next_run, chat, schedule.enabled, schedule.last_crawl],
        )?;
        Ok(schedule.id)
    }

    pub fn delete_schedule(&self, id: i64) -> Result<()> {
        self.execute("DELETE FROM schedules WHERE id = ?1", &params![id])?;
        Ok(())
    }

    /// The crawl before `crawl_id` of the same start URL that finished.
    pub fn previous_crawl(&self, crawl_id: i64) -> Result<Option<i64>> {
        let row = self.query_row(
            "SELECT id FROM crawls WHERE start_url = (SELECT start_url FROM crawls WHERE id = ?1) \
             AND id < ?1 AND finished_at IS NOT NULL ORDER BY id DESC LIMIT 1",
            &params![crawl_id],
        )?;
        row.map(|row| row.get(0)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keeps(target: &str) {
        let store = crate::tests::empty(target);
        let mut schedule = Schedule {
            id: 0,
            request: serde_json::json!({"start_url": "https://e.com/", "preset": "seo"}),
            every_hours: 24,
            next_run: 1_800_000_000,
            telegram_chat: Some(" -100123 ".into()),
            enabled: true,
            last_crawl: None,
        };
        schedule.id = store.save_schedule(&schedule).unwrap();
        assert!(schedule.id > 0);
        schedule.telegram_chat = Some("-100123".into());
        assert_eq!(store.schedules().unwrap(), [schedule.clone()]);
        schedule.enabled = false;
        schedule.last_crawl = Some(7);
        store.save_schedule(&schedule).unwrap();
        assert_eq!(store.schedule(schedule.id).unwrap(), Some(schedule.clone()));
        store.delete_schedule(schedule.id).unwrap();
        assert!(store.schedules().unwrap().is_empty());

        let first = store.start_crawl("https://e.com/", "{}").unwrap();
        let other = store.start_crawl("https://other.com/", "{}").unwrap();
        let unfinished = store.start_crawl("https://e.com/", "{}").unwrap();
        let last = store.start_crawl("https://e.com/", "{}").unwrap();
        for id in [first, other, last] {
            store.finish_crawl(id, false).unwrap();
        }
        assert_eq!(store.previous_crawl(last).unwrap(), Some(first));
        assert_eq!(store.previous_crawl(unfinished).unwrap(), Some(first));
        assert_eq!(store.previous_crawl(first).unwrap(), None);
    }

    #[test]
    fn keeps_schedules() {
        let path = std::env::temp_dir().join(format!("seo-store-schedule-{}.sqlite", std::process::id()));
        keeps(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn keeps_schedules_on_servers() {
        let _one = crate::tests::SERVER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for target in crate::tests::servers() {
            keeps(&target);
        }
    }
}
