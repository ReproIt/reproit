//! tokio-postgres through the exchange boundary (feature `pg`).
//!
//! The one canonical DB driver, like `pg` in Node and psycopg in Python.
//! [`connect`] returns a [`Client`] whose [`Client::query`] and
//! [`Client::execute`] route every statement through `instrument::db::run`,
//! so statements and their results are recorded as `pg` exchanges on the
//! ambient trace in exactly the wire shape the Node reference emits:
//! request `{text, values}`, response `{command, rowCount, rows}` or
//! `{error: {message, code}}`, rows bounded at 64.
//!
//! With `REPROIT_REPLAY` set, [`connect`] is a CONNECT STUB: no server is
//! dialed at all, so the application boots with the database down, and every
//! statement is served from the recorded exchanges (a statement the capture
//! never saw fails closed with the divergence marker).
//!
//! Named capability gaps, not silent downgrades:
//! - only statements routed through this wrapper are captured; transactions
//!   driven through the raw `tokio_postgres::Client`, COPY, LISTEN/NOTIFY
//!   and prepared portals are invisible to capture and unavailable at
//!   replay;
//! - parameters bind for JSON scalars (null, bool, integer, float, string);
//!   any other parameter value is refused loudly;
//! - result columns of types outside bool / int2/4/8 / float4/8 / text
//!   kinds / json(b) record as null, and the `command` tag is derived from
//!   the statement verb (tokio-postgres does not surface the server tag on
//!   the query path).

use crate::instrument::db::{self, DbError, DbOutcome};
use crate::instrument::{self, replaying};
use serde_json::{json, Value};
use tokio_postgres::types::{ToSql, Type};

/// A replay-aware tokio-postgres handle. In replay mode `inner` is `None`:
/// nothing was dialed and `db::run` serves every statement.
pub struct Client {
    inner: Option<tokio_postgres::Client>,
}

/// Connect to postgres, or return the connect stub in replay mode. The
/// spawned task drives the connection exactly as tokio-postgres documents.
pub async fn connect(conninfo: &str) -> Result<Client, DbError> {
    instrument::init();
    if replaying() {
        return Ok(Client { inner: None });
    }
    let (client, connection) = tokio_postgres::connect(conninfo, tokio_postgres::NoTls)
        .await
        .map_err(to_db_error)?;
    tokio::spawn(async move {
        // Connection errors surface on the statement that observes them.
        let _ = connection.await;
    });
    Ok(Client {
        inner: Some(client),
    })
}

impl Client {
    /// Run one statement, returning rows as JSON objects keyed by column
    /// name (the Node `pg` row shape).
    pub async fn query(&self, text: &str, values: &[Value]) -> Result<DbOutcome, DbError> {
        db::run(text, values, || async {
            let client = self.live()?;
            let params = to_params(values)?;
            let refs: Vec<&(dyn ToSql + Sync)> = params
                .iter()
                .map(|param| param.as_ref() as &(dyn ToSql + Sync))
                .collect();
            let rows = client.query(text, &refs).await.map_err(to_db_error)?;
            Ok(DbOutcome {
                command: Some(statement_command(text)),
                row_count: rows.len() as u64,
                rows: rows.iter().map(row_to_json).collect(),
            })
        })
        .await
    }

    /// Run one statement for its affected-row count (INSERT/UPDATE/DELETE).
    pub async fn execute(&self, text: &str, values: &[Value]) -> Result<DbOutcome, DbError> {
        db::run(text, values, || async {
            let client = self.live()?;
            let params = to_params(values)?;
            let refs: Vec<&(dyn ToSql + Sync)> = params
                .iter()
                .map(|param| param.as_ref() as &(dyn ToSql + Sync))
                .collect();
            let count = client.execute(text, &refs).await.map_err(to_db_error)?;
            Ok(DbOutcome {
                command: Some(statement_command(text)),
                row_count: count,
                rows: Vec::new(),
            })
        })
        .await
    }

    fn live(&self) -> Result<&tokio_postgres::Client, DbError> {
        // db::run never calls the live closure in replay mode, so a missing
        // inner client here is a boundary violation, not a user error.
        self.inner.as_ref().ok_or_else(|| DbError {
            message: "reproit: live database reached during hermetic replay".into(),
            code: None,
        })
    }
}

fn to_db_error(error: tokio_postgres::Error) -> DbError {
    DbError {
        code: error
            .as_db_error()
            .map(|db_error| db_error.code().code().to_string()),
        message: error.to_string(),
    }
}

/// The statement verb, standing in for the server command tag the query
/// path does not surface (named gap in the module docs).
fn statement_command(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// JSON scalars to postgres parameters. Anything else is refused loudly:
/// half-recording a parameter the matcher would then mis-compare is worse
/// than an error at the call site.
fn to_params(values: &[Value]) -> Result<Vec<Box<dyn ToSql + Sync + Send>>, DbError> {
    values
        .iter()
        .map(|value| -> Result<Box<dyn ToSql + Sync + Send>, DbError> {
            match value {
                Value::Null => Ok(Box::new(Option::<String>::None)),
                Value::Bool(flag) => Ok(Box::new(*flag)),
                Value::Number(number) => match number.as_i64() {
                    Some(integer) => Ok(Box::new(integer)),
                    None => Ok(Box::new(number.as_f64().unwrap_or(0.0))),
                },
                Value::String(text) => Ok(Box::new(text.clone())),
                other => Err(DbError {
                    message: format!("reproit pg: unsupported parameter value {other}"),
                    code: None,
                }),
            }
        })
        .collect()
}

/// One row as a JSON object keyed by column name, for the bounded set of
/// column types the module docs name; anything else records as null.
fn row_to_json(row: &tokio_postgres::Row) -> Value {
    let mut fields = serde_json::Map::new();
    for (index, column) in row.columns().iter().enumerate() {
        let value = match *column.type_() {
            Type::BOOL => row
                .try_get::<_, Option<bool>>(index)
                .ok()
                .flatten()
                .map(Value::from),
            Type::INT2 => row
                .try_get::<_, Option<i16>>(index)
                .ok()
                .flatten()
                .map(Value::from),
            Type::INT4 => row
                .try_get::<_, Option<i32>>(index)
                .ok()
                .flatten()
                .map(Value::from),
            Type::INT8 => row
                .try_get::<_, Option<i64>>(index)
                .ok()
                .flatten()
                .map(Value::from),
            Type::FLOAT4 => row
                .try_get::<_, Option<f32>>(index)
                .ok()
                .flatten()
                .map(|value| json!(value)),
            Type::FLOAT8 => row
                .try_get::<_, Option<f64>>(index)
                .ok()
                .flatten()
                .map(|value| json!(value)),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => row
                .try_get::<_, Option<String>>(index)
                .ok()
                .flatten()
                .map(Value::String),
            Type::JSON | Type::JSONB => row.try_get::<_, Option<Value>>(index).ok().flatten(),
            _ => None,
        };
        fields.insert(column.name().to_string(), value.unwrap_or(Value::Null));
    }
    Value::Object(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_bind_json_scalars_and_refuse_the_rest() {
        assert_eq!(
            to_params(&[json!(null), json!(true), json!(7), json!(2.5), json!("a")])
                .expect("scalars")
                .len(),
            5
        );
        let refused = to_params(&[json!({"nested": 1})]);
        assert!(refused.is_err(), "structured parameters are refused loudly");
    }

    #[test]
    fn the_command_tag_is_the_statement_verb() {
        assert_eq!(statement_command("select 1"), "SELECT");
        assert_eq!(statement_command("  INSERT INTO t VALUES (1)"), "INSERT");
    }
}
