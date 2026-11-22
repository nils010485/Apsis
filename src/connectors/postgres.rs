use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use either::Either;
use futures::TryStreamExt;
use serde_json::{Map, Value, json};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow, PgValueFormat};
use sqlx::{AssertSqlSafe, Column, Row, TypeInfo, ValueRef};
use uuid::Uuid;

use crate::config::PresetConfig;
use crate::error::{ApsisError, Result};

const DEFAULT_PG_MAX_CONNECTIONS: u32 = 5;

/// Build a lazy PostgreSQL connection pool for a preset.
pub fn build_pool(preset: &PresetConfig) -> Result<PgPool> {
    let PresetConfig::Postgres {
        host,
        port,
        user,
        password,
        database,
    } = preset
    else {
        return Err(ApsisError::InvalidParams(format!(
            "Preset protocol '{}' is not PostgreSQL",
            preset.protocol()
        )));
    };

    let opts = PgConnectOptions::new()
        .host(host)
        .port(*port)
        .username(user)
        .password(password)
        .database(database);

    Ok(PgPoolOptions::new()
        .max_connections(DEFAULT_PG_MAX_CONNECTIONS)
        .connect_lazy_with(opts))
}

/// Execute a raw SQL query against a PostgreSQL pool and serialize the result as JSON.
pub async fn execute_query(pool: &PgPool, sql: &str) -> Result<String> {
    if sql.trim().is_empty() {
        return Err(ApsisError::InvalidParams(
            "SQL query cannot be empty".to_string(),
        ));
    }

    let mut rows_out: Vec<Value> = Vec::new();
    let mut total_affected: u64 = 0;
    let mut saw_query_result = false;

    let mut stream = sqlx::raw_sql(AssertSqlSafe(sql.to_string())).fetch_many(pool);
    while let Some(item) = stream
        .try_next()
        .await
        .map_err(|e| ApsisError::Connector(format!("PostgreSQL query error: {e}")))?
    {
        match item {
            Either::Left(result) => {
                saw_query_result = true;
                total_affected += result.rows_affected();
            }
            Either::Right(row) => {
                rows_out.push(pg_row_to_json(&row)?);
            }
        }
    }

    let payload = if !rows_out.is_empty() || is_row_returning_sql(sql) || !saw_query_result {
        Value::Array(rows_out)
    } else {
        json!({ "rows_affected": total_affected })
    };

    serde_json::to_string_pretty(&payload)
        .map_err(|e| ApsisError::Connector(format!("Failed to serialize PostgreSQL rows: {e}")))
}

fn is_row_returning_sql(sql: &str) -> bool {
    let first_word = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        first_word.as_str(),
        "SELECT" | "WITH" | "SHOW" | "EXPLAIN" | "VALUES" | "TABLE"
    )
}

fn pg_row_to_json(row: &PgRow) -> Result<Value> {
    let mut obj = Map::new();
    for (idx, col) in row.columns().iter().enumerate() {
        let val = pg_cell_to_json(row, idx, col.type_info().name())?;
        obj.insert(col.name().to_string(), val);
    }
    Ok(Value::Object(obj))
}

fn pg_cell_to_json(row: &PgRow, idx: usize, type_name: &str) -> Result<Value> {
    let raw = row
        .try_get_raw(idx)
        .map_err(|e| ApsisError::Connector(format!("Failed to read column {idx}: {e}")))?;

    if raw.is_null() {
        return Ok(Value::Null);
    }

    let decoded = match type_name {
        "BOOL" => row.try_get::<bool, _>(idx).ok().map(Value::Bool),
        "INT2" => row.try_get::<i16, _>(idx).ok().map(|v| json!(v)),
        "INT4" => row.try_get::<i32, _>(idx).ok().map(|v| json!(v)),
        "INT8" => row.try_get::<i64, _>(idx).ok().map(|v| json!(v)),
        "FLOAT4" => row.try_get::<f32, _>(idx).ok().map(|v| {
            if v.is_finite() {
                json!(v)
            } else {
                Value::String(v.to_string())
            }
        }),
        "FLOAT8" => row.try_get::<f64, _>(idx).ok().map(|v| {
            if v.is_finite() {
                json!(v)
            } else {
                Value::String(v.to_string())
            }
        }),
        "JSON" | "JSONB" => row.try_get::<Value, _>(idx).ok(),
        "UUID" => row
            .try_get::<Uuid, _>(idx)
            .ok()
            .map(|u| Value::String(u.to_string())),
        "DATE" => row
            .try_get::<NaiveDate, _>(idx)
            .ok()
            .map(|d| Value::String(d.to_string())),
        "TIME" => row
            .try_get::<NaiveTime, _>(idx)
            .ok()
            .map(|t| Value::String(t.to_string())),
        "TIMESTAMP" => row
            .try_get::<NaiveDateTime, _>(idx)
            .ok()
            .map(|dt| Value::String(dt.to_string())),
        "TIMESTAMPTZ" => row
            .try_get::<DateTime<Utc>, _>(idx)
            .ok()
            .map(|dt| Value::String(dt.to_rfc3339())),
        "BYTEA" => row.try_get::<Vec<u8>, _>(idx).ok().map(|bytes| {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            Value::String(format!("\\x{hex}"))
        }),
        _ => None,
    };

    if let Some(v) = decoded {
        return Ok(v);
    }

    if raw.format() == PgValueFormat::Text
        && let Ok(s) = raw.as_str()
    {
        return Ok(Value::String(s.to_string()));
    }

    if let Ok(s) = row.try_get::<String, _>(idx) {
        return Ok(Value::String(s));
    }

    let bytes = raw
        .as_bytes()
        .map_err(|e| ApsisError::Connector(format!("Failed to decode column {idx}: {e}")))?;
    Ok(Value::String(String::from_utf8_lossy(bytes).into_owned()))
}
