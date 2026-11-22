use mongodb::bson::{self, Bson, Document};
use mongodb::options::{ClientOptions, Credential};
use mongodb::{Client, Database};
use serde::Deserialize;
use serde_json::Value;

use crate::config::PresetConfig;
use crate::error::{ApsisError, Result};

#[derive(Debug, Deserialize)]
struct MongoQuery {
    #[serde(default)]
    database: Option<String>,
    command: Value,
}

pub async fn build_client(preset: &PresetConfig) -> Result<Client> {
    let PresetConfig::Mongodb {
        host,
        port,
        user,
        password,
        database: default_database,
        uri,
    } = preset
    else {
        return Err(ApsisError::InvalidParams(format!(
            "Preset protocol '{}' is not MongoDB",
            preset.protocol()
        )));
    };

    let mut client_options = if let Some(raw_uri) = uri.as_deref().filter(|s| !s.trim().is_empty())
    {
        ClientOptions::parse(raw_uri)
            .await
            .map_err(|e| ApsisError::Connector(format!("Invalid MongoDB URI: {e}")))?
    } else {
        let mut opts = ClientOptions::parse(format!("mongodb://{host}:{port}"))
            .await
            .map_err(|e| ApsisError::Connector(format!("Invalid MongoDB options: {e}")))?;
        if let Some(username) = user.as_deref().filter(|s| !s.is_empty()) {
            opts.credential = Some(
                Credential::builder()
                    .username(username.to_string())
                    .password(password.clone())
                    .source(default_database.clone())
                    .build(),
            );
        }
        opts
    };

    if client_options.app_name.is_none() {
        client_options.app_name = Some("apsis".to_string());
    }

    Client::with_options(client_options)
        .map_err(|e| ApsisError::Connector(format!("MongoDB client error: {e}")))
}

pub async fn execute_query(
    client: &Client,
    default_database: Option<&str>,
    query: &Value,
) -> Result<String> {
    let parsed: MongoQuery = match query {
        Value::String(raw) => serde_json::from_str(raw).map_err(|e| {
            ApsisError::InvalidParams(format!("Invalid MongoDB JSON query string: {e}"))
        })?,
        Value::Object(_) => serde_json::from_value(query.clone())
            .map_err(|e| ApsisError::InvalidParams(format!("Invalid MongoDB query object: {e}")))?,
        _ => {
            return Err(ApsisError::InvalidParams(
                "MongoDB query must be a JSON object { database?, command }".to_string(),
            ));
        }
    };

    let db_name = parsed
        .database
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| default_database.map(str::trim).filter(|s| !s.is_empty()))
        .ok_or_else(|| {
            ApsisError::InvalidParams(
                "Missing 'database' in MongoDB query (and no default database configured on preset)"
                    .to_string(),
            )
        })?
        .to_string();

    let db = client.database(&db_name);
    let command_doc = json_to_doc(parsed.command)?;

    let mut reply = db
        .run_command(command_doc)
        .await
        .map_err(|e| ApsisError::Connector(format!("MongoDB command failed: {e}")))?;

    drain_cursor(&db, &db_name, &mut reply).await?;

    let out = Bson::Document(reply).into_relaxed_extjson();
    serde_json::to_string_pretty(&out)
        .map_err(|e| ApsisError::Connector(format!("Failed to serialize MongoDB output: {e}")))
}

/// Drain a live cursor via getMore, merging batches into firstBatch and resetting id to 0.
async fn drain_cursor(db: &Database, db_name: &str, reply: &mut Document) -> Result<()> {
    let Ok(cursor) = reply.get_document_mut("cursor") else {
        return Ok(());
    };
    let mut id = bson_int(cursor.get("id"));
    if id == 0 {
        return Ok(());
    }

    let ns = cursor.get_str("ns").unwrap_or_default().to_string();
    let prefix = format!("{db_name}.");
    let collection = ns.strip_prefix(&prefix).unwrap_or(&ns).to_string();

    let mut docs: Vec<Bson> = cursor
        .get_array("firstBatch")
        .map(|batch| batch.to_vec())
        .unwrap_or_default();

    while id != 0 {
        let get_more = bson::doc! {
            "getMore": id,
            "collection": &collection,
            "batchSize": 1000,
        };
        let more = db
            .run_command(get_more)
            .await
            .map_err(|e| ApsisError::Connector(format!("MongoDB getMore failed: {e}")))?;
        let next = more.get_document("cursor").map_err(|e| {
            ApsisError::Connector(format!("MongoDB getMore reply missing cursor: {e}"))
        })?;
        if let Ok(batch) = next.get_array("nextBatch") {
            docs.extend(batch.iter().cloned());
        }
        id = bson_int(next.get("id"));
    }

    cursor.insert("firstBatch", docs);
    cursor.insert("id", 0i64);
    Ok(())
}

fn bson_int(value: Option<&Bson>) -> i64 {
    match value {
        Some(Bson::Int64(n)) => *n,
        Some(Bson::Int32(n)) => i64::from(*n),
        _ => 0,
    }
}

fn json_to_doc(value: Value) -> Result<Document> {
    if !value.is_object() {
        return Err(ApsisError::InvalidParams(
            "Expected a JSON object for MongoDB document".to_string(),
        ));
    }
    bson::to_document(&value)
        .map_err(|e| ApsisError::InvalidParams(format!("Failed to convert JSON to BSON: {e}")))
}
