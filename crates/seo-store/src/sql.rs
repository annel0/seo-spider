//! What the store asks of a database, and nothing more: run a statement,
//! stream the rows of a query, put many rows into a table, set columns of
//! many rows, and transactions. `Backend` is that, once per database
//! (SQLite, PostgreSQL, MySQL); the store's logic and its SQL are written
//! once on top of it.
//!
//! SQL is written for all three: standard SQL, parameters as `?1`, `?2`
//! (repeatable), and the few spellings that differ translated by
//! `translate` (`$1` for PostgreSQL; `?` in order, and `char_length` for
//! `length`, for MySQL).

use std::borrow::Cow;

use anyhow::{anyhow, bail, Result};

/// Which database a backend speaks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
    Mysql,
}

/// A statement parameter.
#[derive(Debug, Clone)]
pub enum Param<'a> {
    Null,
    Int(i64),
    Real(f64),
    Text(Cow<'a, str>),
    Blob(&'a [u8]),
}

impl From<i64> for Param<'_> {
    fn from(value: i64) -> Self {
        Param::Int(value)
    }
}

impl From<i32> for Param<'_> {
    fn from(value: i32) -> Self {
        Param::Int(value.into())
    }
}

impl From<u16> for Param<'_> {
    fn from(value: u16) -> Self {
        Param::Int(value.into())
    }
}

impl From<u32> for Param<'_> {
    fn from(value: u32) -> Self {
        Param::Int(value.into())
    }
}

impl From<u64> for Param<'_> {
    fn from(value: u64) -> Self {
        Param::Int(value as i64)
    }
}

impl From<usize> for Param<'_> {
    fn from(value: usize) -> Self {
        Param::Int(value as i64)
    }
}

impl From<bool> for Param<'_> {
    fn from(value: bool) -> Self {
        Param::Int(value.into())
    }
}

impl From<f64> for Param<'_> {
    fn from(value: f64) -> Self {
        Param::Real(value)
    }
}

impl<'a> From<&'a str> for Param<'a> {
    fn from(value: &'a str) -> Self {
        Param::Text(Cow::Borrowed(value))
    }
}

impl<'a> From<&'a String> for Param<'a> {
    fn from(value: &'a String) -> Self {
        Param::Text(Cow::Borrowed(value))
    }
}

impl From<String> for Param<'_> {
    fn from(value: String) -> Self {
        Param::Text(Cow::Owned(value))
    }
}

impl<'a> From<&'a [u8]> for Param<'a> {
    fn from(value: &'a [u8]) -> Self {
        Param::Blob(value)
    }
}

impl<'a> From<&'a Vec<u8>> for Param<'a> {
    fn from(value: &'a Vec<u8>) -> Self {
        Param::Blob(value)
    }
}

impl<'a, T: Into<Param<'a>>> From<Option<T>> for Param<'a> {
    fn from(value: Option<T>) -> Self {
        value.map_or(Param::Null, Into::into)
    }
}

/// `[p1, p2, ...]` as parameters.
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        [$($crate::Param::from($value)),*]
    };
}

/// A value a query returned.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A row a query returned. MySQL hands text and numbers of some types back
/// as bytes; the conversions read them as the type asked for.
#[derive(Debug, Clone)]
pub struct Row(pub Vec<Value>);

impl Row {
    pub fn get<T: FromValue>(&self, at: usize) -> Result<T> {
        let value = self.0.get(at).ok_or_else(|| anyhow!("no column {at}"))?;
        T::from_value(value).map_err(|error| anyhow!("column {at}: {error}"))
    }

    /// The value itself, left `Null`: for blobs, without a copy.
    pub fn take(&mut self, at: usize) -> Value {
        self.0.get_mut(at).map_or(Value::Null, |value| std::mem::replace(value, Value::Null))
    }

    /// A text column, taken out of the row without a copy.
    pub fn take_text(&mut self, at: usize) -> Result<Option<String>> {
        match self.take(at) {
            Value::Null => Ok(None),
            Value::Text(text) => Ok(Some(text)),
            Value::Blob(bytes) => Ok(Some(String::from_utf8(bytes)?)),
            Value::Int(value) => Ok(Some(value.to_string())),
            Value::Real(value) => Ok(Some(value.to_string())),
        }
    }

    /// A text column that may not be null, taken out of the row.
    pub fn take_string(&mut self, at: usize) -> Result<String> {
        self.take_text(at)?.ok_or_else(|| anyhow!("column {at}: null where text was expected"))
    }

    /// A blob column, taken out of the row.
    pub fn take_blob(&mut self, at: usize) -> Result<Option<Vec<u8>>> {
        match self.take(at) {
            Value::Null => Ok(None),
            Value::Blob(bytes) => Ok(Some(bytes)),
            Value::Text(text) => Ok(Some(text.into_bytes())),
            other => bail!("column {at}: {other:?} is not a blob"),
        }
    }
}

pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Result<Self>;
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Int(value) => Ok(*value),
            Value::Real(value) => Ok(value.round() as i64),
            Value::Text(text) => parse_number(text),
            Value::Blob(bytes) => parse_number(std::str::from_utf8(bytes)?),
            Value::Null => bail!("null where a number was expected"),
        }
    }
}

/// A number MySQL sent as text: `42`, or a DECIMAL sum as `42.0000`.
fn parse_number(text: &str) -> Result<i64> {
    let text = text.trim();
    text.parse::<i64>().or_else(|_| text.parse::<f64>().map(|value| value.round() as i64)).map_err(|_| anyhow!("{text:?} is not a number"))
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Int(value) => Ok(*value as f64),
            Value::Real(value) => Ok(*value),
            Value::Text(text) => Ok(text.trim().parse()?),
            Value::Blob(bytes) => Ok(std::str::from_utf8(bytes)?.trim().parse()?),
            Value::Null => bail!("null where a number was expected"),
        }
    }
}

macro_rules! from_integer {
    ($($type:ty),*) => {$(
        impl FromValue for $type {
            fn from_value(value: &Value) -> Result<Self> {
                let wide = i64::from_value(value)?;
                <$type>::try_from(wide).map_err(|_| anyhow!("{wide} out of range"))
            }
        }
    )*};
}

from_integer!(u16, u32, u64, usize, i32);

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self> {
        Ok(i64::from_value(value)? != 0)
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Text(text) => Ok(text.clone()),
            Value::Blob(bytes) => Ok(String::from_utf8(bytes.clone())?),
            Value::Int(value) => Ok(value.to_string()),
            Value::Real(value) => Ok(value.to_string()),
            Value::Null => bail!("null where text was expected"),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Blob(bytes) => Ok(bytes.clone()),
            Value::Text(text) => Ok(text.clone().into_bytes()),
            other => bail!("{other:?} is not a blob"),
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(None),
            value => T::from_value(value).map(Some),
        }
    }
}

/// How a column is stored, for the bulk paths that need its type (COPY).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Int,
    Text,
    Blob,
}

/// Columns a bulk insert or update writes, in the order of the values.
#[derive(Debug, Clone, Copy)]
pub struct Columns<'a> {
    pub table: &'a str,
    pub columns: &'a [(&'a str, Kind)],
}

impl Columns<'_> {
    pub fn names(&self) -> String {
        self.columns.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
    }
}

/// One database connection, as the store uses it.
pub trait Backend: Send {
    fn dialect(&self) -> Dialect;

    /// Statements without parameters, separated by `;` (the schema).
    fn batch(&mut self, sql: &str) -> Result<()>;

    /// A statement; how many rows it changed.
    fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64>;

    /// The rows of a query, one at a time as the database sends them.
    fn query(&mut self, sql: &str, params: &[Param], each: &mut dyn FnMut(Row) -> Result<()>) -> Result<()>;

    /// Inserts one row and returns the id the database gave it.
    fn insert_returning_id(&mut self, sql: &str, params: &[Param]) -> Result<i64>;

    /// Many rows into one table, as fast as the database takes them.
    fn insert_rows(&mut self, into: &Columns, rows: &[Vec<Param>]) -> Result<()>;

    /// Sets columns of many rows: the first column of `of` is the key that
    /// finds each row, the others are set.
    fn update_rows(&mut self, of: &Columns, rows: &[Vec<Param>]) -> Result<()>;

    fn begin(&mut self) -> Result<()>;
    fn commit(&mut self) -> Result<()>;
    fn rollback(&mut self) -> Result<()>;

    /// Whether an index of the table exists (MySQL cannot create one only
    /// if it is missing).
    fn index_exists(&mut self, table: &str, index: &str) -> Result<bool>;
}

/// SQL written for SQLite (`?1` parameters) as `dialect` takes it, and for
/// MySQL, whose parameters are positional only, which parameter each `?`
/// stands for.
pub fn translate(sql: &str, dialect: Dialect) -> (Cow<'_, str>, Vec<usize>) {
    if dialect == Dialect::Sqlite {
        return (Cow::Borrowed(sql), Vec::new());
    }
    let mut out = String::with_capacity(sql.len() + 16);
    let mut order = Vec::new();
    let mut chars = sql.char_indices().peekable();
    let mut quoted = false;
    while let Some((at, ch)) = chars.next() {
        if ch == '\'' {
            quoted = !quoted;
            out.push(ch);
            continue;
        }
        if !quoted && ch == '?' {
            let mut digits = String::new();
            while let Some(&(_, digit)) = chars.peek().filter(|(_, digit)| digit.is_ascii_digit()) {
                digits.push(digit);
                chars.next();
            }
            let number: usize = digits.parse().unwrap_or(order.len() + 1);
            match dialect {
                Dialect::Postgres => out.push_str(&format!("${number}")),
                _ => {
                    out.push('?');
                    order.push(number - 1);
                }
            }
            continue;
        }
        if !quoted && dialect == Dialect::Mysql && sql[at..].starts_with("length(") && !out.ends_with('_') {
            out.push_str("char_");
        }
        out.push(ch);
    }
    (Cow::Owned(out), order)
}

/// A database password from the environment, for a URL that has none:
/// `SEO_SPIDER_DB_PASSWORD`, or the database's own variable.
pub fn password_from_env(own: &str) -> Option<String> {
    std::env::var("SEO_SPIDER_DB_PASSWORD").ok().or_else(|| std::env::var(own).ok()).filter(|password| !password.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_parameters_and_lengths() {
        let sql = "SELECT length(title) FROM t WHERE a = ?1 AND b LIKE '?2%' AND c = ?2 AND d = ?1";
        let (postgres, _) = translate(sql, Dialect::Postgres);
        assert_eq!(postgres, "SELECT length(title) FROM t WHERE a = $1 AND b LIKE '?2%' AND c = $2 AND d = $1");
        let (mysql, order) = translate(sql, Dialect::Mysql);
        assert_eq!(mysql, "SELECT char_length(title) FROM t WHERE a = ? AND b LIKE '?2%' AND c = ? AND d = ?");
        assert_eq!(order, [0, 1, 0]);
        assert_eq!(translate("SELECT char_length(x)", Dialect::Mysql).0, "SELECT char_length(x)");
        assert_eq!(translate(sql, Dialect::Sqlite).0, sql);
    }

    #[test]
    fn reads_numbers_mysql_sends_as_text() {
        assert_eq!(i64::from_value(&Value::Blob(b"42".to_vec())).unwrap(), 42);
        assert_eq!(i64::from_value(&Value::Blob(b"1234.0000".to_vec())).unwrap(), 1234);
        assert_eq!(String::from_value(&Value::Blob("ёж".as_bytes().to_vec())).unwrap(), "ёж");
        assert_eq!(Option::<u32>::from_value(&Value::Null).unwrap(), None);
        assert!(bool::from_value(&Value::Int(1)).unwrap());
    }
}
