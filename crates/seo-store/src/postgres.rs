//! PostgreSQL. The connection is driven on a thread of its own by a small
//! runtime; the store's calls run its futures with a plain `block_on`, so
//! they work from any thread, a crawl's async tasks included (the `postgres`
//! crate's own runtime would refuse to start inside them). Bulk inserts are
//! `COPY ... FROM STDIN BINARY`; bulk updates go through a temporary table.

use std::collections::HashMap;
use std::error::Error;

use anyhow::{anyhow, Context, Result};
use bytes::BytesMut;
use futures_executor::block_on;
use futures_util::{pin_mut, StreamExt};
use tokio_postgres::binary_copy::BinaryCopyInWriter;
use tokio_postgres::types::{to_sql_checked, FromSql, IsNull, ToSql, Type};
use tokio_postgres::{Client, Statement};

use crate::sql::{password_from_env, translate, Backend, Columns, Dialect, Kind, Param, Row, Value};

pub struct Postgres {
    client: Client,
    statements: HashMap<String, Statement>,
}

/// Prepared statements kept per connection; the UI's tables make a few
/// hundred kinds at most.
const STATEMENTS: usize = 512;

impl Postgres {
    pub fn connect(url: &str) -> Result<Self> {
        let mut config: tokio_postgres::Config = url.parse().context("PostgreSQL address")?;
        if config.get_password().is_none() {
            if let Some(password) = password_from_env("PGPASSWORD") {
                config.password(password);
            }
        }
        config.application_name("seo-spider");
        let tls = tls()?;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("postgres".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = sender.send(Err(anyhow!(error)));
                    return;
                }
            };
            runtime.block_on(async move {
                match config.connect(tls).await {
                    Ok((client, connection)) => {
                        let _ = sender.send(Ok(client));
                        // Runs until the client is dropped.
                        if let Err(error) = connection.await {
                            tracing::warn!(%error, "PostgreSQL connection");
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(anyhow!(error)));
                    }
                }
            });
        })?;
        let client = receiver.recv().map_err(|_| anyhow!("PostgreSQL connection thread ended"))?.context("connect to PostgreSQL")?;
        Ok(Postgres { client, statements: HashMap::new() })
    }

    fn statement(&mut self, sql: &str) -> Result<Statement> {
        if let Some(statement) = self.statements.get(sql) {
            return Ok(statement.clone());
        }
        let (translated, _) = translate(sql, Dialect::Postgres);
        let statement = block_on(self.client.prepare(&translated)).with_context(|| format!("prepare {translated}"))?;
        if self.statements.len() >= STATEMENTS {
            self.statements.clear();
        }
        self.statements.insert(sql.to_string(), statement.clone());
        Ok(statement)
    }

    fn copy_in(&mut self, table: &str, columns: &[(&str, Kind)], rows: &[Vec<Param>]) -> Result<()> {
        let names = columns.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ");
        let types: Vec<Type> = columns
            .iter()
            .map(|(_, kind)| match kind {
                Kind::Int => Type::INT8,
                Kind::Text => Type::TEXT,
                Kind::Blob => Type::BYTEA,
            })
            .collect();
        block_on(async {
            let sink = self.client.copy_in(&format!("COPY {table} ({names}) FROM STDIN BINARY")).await?;
            let writer = BinaryCopyInWriter::new(sink, &types);
            pin_mut!(writer);
            for row in rows {
                let values: Vec<&(dyn ToSql + Sync)> = row.iter().map(|param| param as &(dyn ToSql + Sync)).collect();
                writer.as_mut().write(&values).await?;
            }
            writer.finish().await?;
            Ok::<_, tokio_postgres::Error>(())
        })
        .with_context(|| format!("COPY into {table}"))
    }
}

/// TLS when the server offers it (`sslmode=prefer`, the default), or as the
/// address asks; the roots are the web's.
fn tls() -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

impl ToSql for Param<'_> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            Param::Null => Ok(IsNull::Yes),
            Param::Int(value) => match *ty {
                Type::INT2 => i16::try_from(*value)?.to_sql(ty, out),
                Type::INT4 => i32::try_from(*value)?.to_sql(ty, out),
                Type::FLOAT8 => (*value as f64).to_sql(ty, out),
                Type::FLOAT4 => (*value as f32).to_sql(ty, out),
                Type::BOOL => (*value != 0).to_sql(ty, out),
                Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => value.to_string().to_sql(ty, out),
                _ => value.to_sql(ty, out),
            },
            Param::Real(value) => match *ty {
                Type::FLOAT4 => (*value as f32).to_sql(ty, out),
                _ => value.to_sql(ty, out),
            },
            // PostgreSQL text cannot hold NUL; a page's title can.
            Param::Text(text) if text.contains('\0') => text.replace('\0', "").as_str().to_sql(ty, out),
            Param::Text(text) => text.as_ref().to_sql(ty, out),
            Param::Blob(bytes) => bytes.to_sql(ty, out),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

/// A column of any type the store reads.
struct Any(Value);

impl<'a> FromSql<'a> for Any {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Any(match *ty {
            Type::INT2 => Value::Int(i16::from_sql(ty, raw)?.into()),
            Type::INT4 => Value::Int(i32::from_sql(ty, raw)?.into()),
            Type::INT8 => Value::Int(i64::from_sql(ty, raw)?),
            Type::OID => Value::Int(u32::from_sql(ty, raw)?.into()),
            Type::FLOAT4 => Value::Real(f32::from_sql(ty, raw)?.into()),
            Type::FLOAT8 => Value::Real(f64::from_sql(ty, raw)?),
            Type::BOOL => Value::Int(bool::from_sql(ty, raw)?.into()),
            Type::NUMERIC => numeric(raw)?,
            Type::BYTEA => Value::Blob(raw.to_vec()),
            _ => Value::Text(String::from_utf8(raw.to_vec())?),
        }))
    }

    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Any(Value::Null))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// A NUMERIC (what `sum` and `avg` of integers give) in its binary form:
/// base-10000 digits with a weight; an integer when it has no fraction.
fn numeric(raw: &[u8]) -> Result<Value, Box<dyn Error + Sync + Send>> {
    let word = |at: usize| -> Result<i16, Box<dyn Error + Sync + Send>> {
        Ok(i16::from_be_bytes(raw.get(at..at + 2).ok_or("short NUMERIC")?.try_into()?))
    };
    let (digits, weight, sign, scale) = (word(0)? as usize, word(2)?, word(4)? as u16, word(6)?);
    if sign == 0xC000 {
        return Ok(Value::Real(f64::NAN));
    }
    let mut value = 0f64;
    for at in 0..digits {
        value += f64::from(word(8 + at * 2)?) * 10000f64.powi(i32::from(weight) - at as i32);
    }
    if sign == 0x4000 {
        value = -value;
    }
    Ok(if scale == 0 && value.abs() < 9.0e15 { Value::Int(value as i64) } else { Value::Real(value) })
}

impl Backend for Postgres {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    fn batch(&mut self, sql: &str) -> Result<()> {
        Ok(block_on(self.client.batch_execute(sql))?)
    }

    fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64> {
        let statement = self.statement(sql)?;
        let values: Vec<&(dyn ToSql + Sync)> = params.iter().map(|param| param as &(dyn ToSql + Sync)).collect();
        Ok(block_on(self.client.execute(&statement, &values))?)
    }

    fn query(&mut self, sql: &str, params: &[Param], each: &mut dyn FnMut(Row) -> Result<()>) -> Result<()> {
        let statement = self.statement(sql)?;
        let width = statement.columns().len();
        let values = params.iter().map(|param| param as &(dyn ToSql + Sync));
        let rows = block_on(self.client.query_raw(&statement, values))?;
        pin_mut!(rows);
        while let Some(row) = block_on(rows.next()) {
            let row = row?;
            let values = (0..width).map(|at| row.try_get::<_, Any>(at).map(|any| any.0)).collect::<Result<_, _>>()?;
            each(Row(values))?;
        }
        Ok(())
    }

    fn insert_returning_id(&mut self, sql: &str, params: &[Param]) -> Result<i64> {
        let statement = self.statement(&format!("{sql} RETURNING id"))?;
        let values: Vec<&(dyn ToSql + Sync)> = params.iter().map(|param| param as &(dyn ToSql + Sync)).collect();
        let row = block_on(self.client.query_one(&statement, &values))?;
        Ok(row.try_get(0)?)
    }

    fn insert_rows(&mut self, into: &Columns, rows: &[Vec<Param>]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        self.copy_in(into.table, into.columns, rows)
    }

    fn update_rows(&mut self, of: &Columns, rows: &[Vec<Param>]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let staging = format!("{}_update", of.table);
        let columns = of
            .columns
            .iter()
            .map(|(name, kind)| format!("{name} {}", match kind { Kind::Int => "BIGINT", Kind::Text => "TEXT", Kind::Blob => "BYTEA" }))
            .collect::<Vec<_>>()
            .join(", ");
        block_on(self.client.batch_execute(&format!("DROP TABLE IF EXISTS {staging}; CREATE TEMP TABLE {staging} ({columns})")))?;
        self.copy_in(&staging, of.columns, rows)?;
        let (key, _) = of.columns[0];
        let sets = of.columns[1..].iter().map(|(name, _)| format!("{name} = s.{name}")).collect::<Vec<_>>().join(", ");
        block_on(self.client.batch_execute(&format!(
            "UPDATE {table} t SET {sets} FROM {staging} s WHERE t.{key} = s.{key}; DROP TABLE {staging}",
            table = of.table
        )))?;
        Ok(())
    }

    fn begin(&mut self) -> Result<()> {
        self.batch("BEGIN")
    }

    fn commit(&mut self) -> Result<()> {
        self.batch("COMMIT")
    }

    fn rollback(&mut self) -> Result<()> {
        self.batch("ROLLBACK")
    }

    fn index_exists(&mut self, _table: &str, index: &str) -> Result<bool> {
        let found: i64 = block_on(self.client.query_one("SELECT count(*) FROM pg_indexes WHERE indexname = $1", &[&index]))?.try_get(0)?;
        Ok(found > 0)
    }
}
