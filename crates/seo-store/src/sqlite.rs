//! SQLite: the crawl file. Statements are prepared once per connection and
//! bulk inserts are prepared inserts inside the caller's transaction, which
//! is as fast as SQLite takes rows.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, OpenFlags, ToSql};

use crate::sql::{Backend, Columns, Dialect, Param, Row, Value};

pub struct Sqlite {
    conn: Connection,
}

impl ToSql for Param<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Param::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            Param::Int(value) => ToSqlOutput::Borrowed(ValueRef::Integer(*value)),
            Param::Real(value) => ToSqlOutput::Borrowed(ValueRef::Real(*value)),
            Param::Text(text) => ToSqlOutput::Borrowed(ValueRef::Text(text.as_bytes())),
            Param::Blob(bytes) => ToSqlOutput::Borrowed(ValueRef::Blob(bytes)),
        })
    }
}

impl Sqlite {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        Ok(Sqlite { conn })
    }

    /// Read only, alongside a crawl writing the file (WAL lets readers in
    /// while the writer commits).
    pub fn open_read(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_context(|| format!("open {} for reading", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Sqlite { conn })
    }
}

fn value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Int(value),
        ValueRef::Real(value) => Value::Real(value),
        ValueRef::Text(text) => Value::Text(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
    }
}

impl Backend for Sqlite {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    fn batch(&mut self, sql: &str) -> Result<()> {
        Ok(self.conn.execute_batch(sql)?)
    }

    fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64> {
        let mut statement = self.conn.prepare_cached(sql)?;
        Ok(statement.execute(rusqlite::params_from_iter(params))? as u64)
    }

    fn query(&mut self, sql: &str, params: &[Param], each: &mut dyn FnMut(Row) -> Result<()>) -> Result<()> {
        let mut statement = self.conn.prepare_cached(sql)?;
        let width = statement.column_count();
        let mut rows = statement.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let values = (0..width).map(|at| row.get_ref(at).map(value)).collect::<rusqlite::Result<_>>()?;
            each(Row(values))?;
        }
        Ok(())
    }

    fn insert_returning_id(&mut self, sql: &str, params: &[Param]) -> Result<i64> {
        self.execute(sql, params)?;
        Ok(self.conn.last_insert_rowid())
    }

    fn insert_rows(&mut self, into: &Columns, rows: &[Vec<Param>]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let marks = (1..=into.columns.len()).map(|at| format!("?{at}")).collect::<Vec<_>>().join(", ");
        let mut statement =
            self.conn.prepare_cached(&format!("INSERT INTO {} ({}) VALUES ({marks})", into.table, into.names()))?;
        for row in rows {
            statement.execute(rusqlite::params_from_iter(row))?;
        }
        Ok(())
    }

    fn update_rows(&mut self, of: &Columns, rows: &[Vec<Param>]) -> Result<()> {
        let (key, _) = of.columns[0];
        let sets = of.columns[1..]
            .iter()
            .enumerate()
            .map(|(at, (name, _))| format!("{name} = ?{}", at + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = self.conn.prepare_cached(&format!("UPDATE {} SET {sets} WHERE {key} = ?1", of.table))?;
        for row in rows {
            statement.execute(rusqlite::params_from_iter(row))?;
        }
        Ok(())
    }

    fn begin(&mut self) -> Result<()> {
        Ok(self.conn.execute_batch("BEGIN")?)
    }

    fn commit(&mut self) -> Result<()> {
        Ok(self.conn.execute_batch("COMMIT")?)
    }

    fn rollback(&mut self) -> Result<()> {
        Ok(self.conn.execute_batch("ROLLBACK")?)
    }

    fn index_exists(&mut self, _table: &str, index: &str) -> Result<bool> {
        let found: i64 = self.conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?1", [index], |row| row.get(0))?;
        Ok(found > 0)
    }
}
