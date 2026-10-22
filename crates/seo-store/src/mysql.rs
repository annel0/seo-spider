//! MySQL (8.0 and later). Bulk inserts are multi-row prepared INSERTs, as
//! many rows a statement as its parameter limit and half the server's
//! `max_allowed_packet` allow, in counts that are powers of two so that the
//! few statements there are stay prepared; bulk updates go through a
//! temporary table.

use anyhow::{Context, Result};
use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, Params, Value as MyValue};

use crate::sql::{password_from_env, translate, Backend, Columns, Dialect, Kind, Param, Row, Value};

pub struct Mysql {
    conn: Conn,
    /// Bytes of values a multi-row INSERT may carry.
    max_bytes: usize,
}

/// A statement takes at most this many parameters (the protocol's limit).
const MAX_PARAMS: usize = 65_535;

impl Mysql {
    /// `url` as the `mysql` crate reads it, plus `sslmode=require`, as for
    /// PostgreSQL: TLS checked against the web's roots (the crate turns TLS
    /// on only with one of its certificate parameters, which still work:
    /// `root_cert_path`, `danger_accept_invalid_certs`).
    pub fn connect(url: &str) -> Result<Self> {
        let mut parsed = url::Url::parse(url).context("MySQL address")?;
        let mut tls = false;
        let kept: Vec<(String, String)> = parsed
            .query_pairs()
            .filter(|(name, value)| {
                if name == "sslmode" {
                    tls = matches!(value.as_ref(), "require" | "verify-ca" | "verify-full");
                    return false;
                }
                true
            })
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        if kept.is_empty() {
            parsed.set_query(None);
        } else {
            parsed.query_pairs_mut().clear().extend_pairs(kept);
        }
        let opts = Opts::from_url(parsed.as_str()).context("MySQL address")?;
        let mut builder = OptsBuilder::from_opts(opts.clone())
            .init(vec!["SET NAMES utf8mb4"])
            .stmt_cache_size(Some(128));
        if tls && opts.get_ssl_opts().is_none() {
            builder = builder.ssl_opts(Some(mysql::SslOpts::default()));
        }
        if opts.get_pass().is_none() {
            if let Some(password) = password_from_env("MYSQL_PWD") {
                builder = builder.pass(Some(password));
            }
        }
        let mut conn = Conn::new(builder).context("connect to MySQL")?;
        let packet: Option<u64> = conn.query_first("SELECT @@max_allowed_packet")?;
        let max_bytes = (packet.unwrap_or(4 << 20) / 2).clamp(1 << 20, 32 << 20) as usize;
        Ok(Mysql { conn, max_bytes })
    }

    fn positional(sql: &str, params: &[Param]) -> (String, Params) {
        let (translated, order) = translate(sql, Dialect::Mysql);
        let params = if order.is_empty() {
            Params::Empty
        } else {
            Params::Positional(order.iter().map(|&at| value(&params[at])).collect())
        };
        (translated.into_owned(), params)
    }
}

fn value(param: &Param) -> MyValue {
    match param {
        Param::Null => MyValue::NULL,
        Param::Int(value) => MyValue::Int(*value),
        Param::Real(value) => MyValue::Double(*value),
        Param::Text(text) => MyValue::Bytes(text.as_bytes().to_vec()),
        Param::Blob(bytes) => MyValue::Bytes(bytes.to_vec()),
    }
}

fn size(row: &[Param]) -> usize {
    row.iter()
        .map(|param| match param {
            Param::Text(text) => text.len(),
            Param::Blob(bytes) => bytes.len(),
            _ => 8,
        })
        .sum()
}

fn column_type(kind: Kind) -> &'static str {
    match kind {
        Kind::Int => "BIGINT",
        Kind::Text => "LONGTEXT",
        Kind::Blob => "LONGBLOB",
    }
}

impl Backend for Mysql {
    fn dialect(&self) -> Dialect {
        Dialect::Mysql
    }

    fn batch(&mut self, sql: &str) -> Result<()> {
        for statement in sql.split(';').map(str::trim).filter(|statement| !statement.is_empty()) {
            self.conn.query_drop(statement).with_context(|| format!("run {statement}"))?;
        }
        Ok(())
    }

    fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64> {
        let (sql, params) = Self::positional(sql, params);
        self.conn.exec_drop(sql, params)?;
        Ok(self.conn.affected_rows())
    }

    fn query(&mut self, sql: &str, params: &[Param], each: &mut dyn FnMut(Row) -> Result<()>) -> Result<()> {
        let (sql, params) = Self::positional(sql, params);
        let result = self.conn.exec_iter(sql, params)?;
        for row in result {
            let values = row?
                .unwrap()
                .into_iter()
                .map(|value| match value {
                    MyValue::NULL => Value::Null,
                    MyValue::Bytes(bytes) => Value::Blob(bytes),
                    MyValue::Int(value) => Value::Int(value),
                    MyValue::UInt(value) => Value::Int(value as i64),
                    MyValue::Float(value) => Value::Real(value.into()),
                    MyValue::Double(value) => Value::Real(value),
                    other => Value::Text(other.as_sql(true).trim_matches('\'').to_string()),
                })
                .collect();
            each(Row(values))?;
        }
        Ok(())
    }

    fn insert_returning_id(&mut self, sql: &str, params: &[Param]) -> Result<i64> {
        self.execute(sql, params)?;
        Ok(self.conn.last_insert_id() as i64)
    }

    fn insert_rows(&mut self, into: &Columns, rows: &[Vec<Param>]) -> Result<()> {
        let width = into.columns.len();
        let group = format!("({})", vec!["?"; width].join(", "));
        let mut start = 0;
        while start < rows.len() {
            let (mut end, mut bytes) = (start, 0);
            while end < rows.len() && (end - start + 1) * width <= MAX_PARAMS {
                let row = size(&rows[end]);
                if end > start && bytes + row > self.max_bytes {
                    break;
                }
                bytes += row;
                end += 1;
            }
            // Down to a power of two: the statement for that count is
            // likely prepared already.
            end = start + 1usize.max(1 << (end - start).ilog2());
            let groups = vec![group.as_str(); end - start].join(", ");
            let values: Vec<MyValue> = rows[start..end].iter().flatten().map(value).collect();
            self.conn
                .exec_drop(format!("INSERT INTO {} ({}) VALUES {groups}", into.table, into.names()), Params::Positional(values))
                .with_context(|| format!("insert into {}", into.table))?;
            start = end;
        }
        Ok(())
    }

    fn update_rows(&mut self, of: &Columns, rows: &[Vec<Param>]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let staging = format!("{}_update", of.table);
        let (key, _) = of.columns[0];
        let columns =
            of.columns.iter().map(|(name, kind)| format!("{name} {}", column_type(*kind))).collect::<Vec<_>>().join(", ");
        self.batch(&format!(
            "DROP TEMPORARY TABLE IF EXISTS {staging}; CREATE TEMPORARY TABLE {staging} ({columns}, PRIMARY KEY ({key}))"
        ))?;
        let staged = Columns { table: &staging, columns: of.columns };
        self.insert_rows(&staged, rows)?;
        let sets = of.columns[1..].iter().map(|(name, _)| format!("t.{name} = s.{name}")).collect::<Vec<_>>().join(", ");
        self.batch(&format!(
            "UPDATE {table} t JOIN {staging} s ON t.{key} = s.{key} SET {sets}; DROP TEMPORARY TABLE {staging}",
            table = of.table
        ))
    }

    fn begin(&mut self) -> Result<()> {
        Ok(self.conn.query_drop("START TRANSACTION")?)
    }

    fn commit(&mut self) -> Result<()> {
        Ok(self.conn.query_drop("COMMIT")?)
    }

    fn rollback(&mut self) -> Result<()> {
        Ok(self.conn.query_drop("ROLLBACK")?)
    }

    fn index_exists(&mut self, table: &str, index: &str) -> Result<bool> {
        let found: Option<i64> = self.conn.exec_first(
            "SELECT count(*) FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = ? AND index_name = ?",
            (table, index),
        )?;
        Ok(found.unwrap_or(0) > 0)
    }
}
