use futures::TryStreamExt;
use mongodb::bson::{self, Bson, Document};
use mongodb::options::{ClientOptions, Credential};
use mongodb::{Client, Collection};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::PresetConfig;
use crate::error::{ApsisError, Result};

#[derive(Debug, Deserialize)]
struct MongoQuery {
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    collection: Option<String>,
    op: String,
    #[serde(default)]
    payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct FindOptionsPayload {
    #[serde(default)]
    filter: Option<Value>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    skip: Option<u64>,
    #[serde(default)]
    sort: Option<Value>,
    #[serde(default)]
    projection: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct UpdatePayload {
    filter: Value,
    update: Value,
    #[serde(default)]
    many: bool,
}

#[derive(Debug, Deserialize)]
struct DeletePayload {
    filter: Value,
    #[serde(default)]
    many: bool,
}

/// Build a reusable MongoDB client from a preset.
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

/// Execute a MongoDB operation encoded as a JSON object (or JSON string) and return JSON output.
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
                "MongoDB query must be a JSON object { database, collection, op, payload }"
                    .to_string(),
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

    let require_collection = || -> Result<Collection<Document>> {
        let name = parsed
            .collection
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ApsisError::InvalidParams("Missing 'collection' in MongoDB query".to_string())
            })?;
        Ok(db.collection::<Document>(name))
    };

    let out: Value = match parsed.op.trim() {
        "list_collections" => {
            let names = db.list_collection_names().await.map_err(|e| {
                ApsisError::Connector(format!("MongoDB list_collections failed: {e}"))
            })?;
            json!(names)
        }
        "find" => {
            let coll = require_collection()?;
            let opts = parse_find_payload(parsed.payload)?;
            let filter_doc = json_to_doc(opts.filter.unwrap_or_else(|| json!({})))?;

            let mut action = coll.find(filter_doc);
            if let Some(l) = opts.limit {
                action = action.limit(l);
            }
            if let Some(s) = opts.skip {
                action = action.skip(s);
            }
            if let Some(sort) = opts.sort {
                action = action.sort(json_to_doc(sort)?);
            }
            if let Some(proj) = opts.projection {
                action = action.projection(json_to_doc(proj)?);
            }

            let mut cursor = action
                .await
                .map_err(|e| ApsisError::Connector(format!("MongoDB find failed: {e}")))?;
            let mut docs = Vec::new();
            while let Some(doc) = cursor
                .try_next()
                .await
                .map_err(|e| ApsisError::Connector(format!("MongoDB cursor error: {e}")))?
            {
                docs.push(Bson::Document(doc).into_relaxed_extjson());
            }
            Value::Array(docs)
        }
        "aggregate" => {
            let coll = require_collection()?;
            let pipeline_val = match parsed.payload {
                Some(Value::Array(arr)) => arr,
                Some(Value::Object(mut map)) => match map.remove("pipeline") {
                    Some(Value::Array(arr)) => arr,
                    _ => {
                        return Err(ApsisError::InvalidParams(
                            "MongoDB aggregate payload must be a pipeline array".to_string(),
                        ));
                    }
                },
                _ => {
                    return Err(ApsisError::InvalidParams(
                        "MongoDB aggregate requires 'payload' as an array of stages".to_string(),
                    ));
                }
            };

            let pipeline: Vec<Document> = pipeline_val
                .into_iter()
                .map(json_to_doc)
                .collect::<Result<Vec<_>>>()?;

            let mut cursor = coll
                .aggregate(pipeline)
                .await
                .map_err(|e| ApsisError::Connector(format!("MongoDB aggregate failed: {e}")))?;
            let mut docs = Vec::new();
            while let Some(doc) = cursor
                .try_next()
                .await
                .map_err(|e| ApsisError::Connector(format!("MongoDB cursor error: {e}")))?
            {
                docs.push(Bson::Document(doc).into_relaxed_extjson());
            }
            Value::Array(docs)
        }
        "insert" => {
            let coll = require_collection()?;
            match parsed.payload {
                Some(Value::Array(items)) => {
                    let docs: Vec<Document> = items
                        .into_iter()
                        .map(json_to_doc)
                        .collect::<Result<Vec<_>>>()?;
                    let res = coll.insert_many(docs).await.map_err(|e| {
                        ApsisError::Connector(format!("MongoDB insert_many failed: {e}"))
                    })?;
                    let ids: Vec<Value> = res
                        .inserted_ids
                        .into_values()
                        .map(Bson::into_relaxed_extjson)
                        .collect();
                    json!({ "inserted_count": ids.len(), "inserted_ids": ids })
                }
                Some(obj @ Value::Object(_)) => {
                    let doc = json_to_doc(obj)?;
                    let res = coll.insert_one(doc).await.map_err(|e| {
                        ApsisError::Connector(format!("MongoDB insert_one failed: {e}"))
                    })?;
                    json!({ "inserted_id": res.inserted_id.into_relaxed_extjson() })
                }
                _ => {
                    return Err(ApsisError::InvalidParams(
                        "MongoDB insert requires 'payload' as a document or array of documents"
                            .to_string(),
                    ));
                }
            }
        }
        "update" => {
            let coll = require_collection()?;
            let payload_val = parsed.payload.ok_or_else(|| {
                ApsisError::InvalidParams(
                    "MongoDB update requires 'payload' with { filter, update, many? }".to_string(),
                )
            })?;
            let upd: UpdatePayload = serde_json::from_value(payload_val).map_err(|e| {
                ApsisError::InvalidParams(format!("Invalid MongoDB update payload: {e}"))
            })?;
            let filter_doc = json_to_doc(upd.filter)?;
            let update_doc = json_to_doc(upd.update)?;

            let res = if upd.many {
                coll.update_many(filter_doc, update_doc).await
            } else {
                coll.update_one(filter_doc, update_doc).await
            }
            .map_err(|e| ApsisError::Connector(format!("MongoDB update failed: {e}")))?;

            json!({
                "matched_count": res.matched_count,
                "modified_count": res.modified_count,
                "upserted_id": res.upserted_id.map(Bson::into_relaxed_extjson),
            })
        }
        "delete" => {
            let coll = require_collection()?;
            let payload_val = parsed.payload.unwrap_or_else(|| json!({}));
            let (filter_val, many) = if payload_val.get("filter").is_some() {
                let del: DeletePayload = serde_json::from_value(payload_val).map_err(|e| {
                    ApsisError::InvalidParams(format!("Invalid MongoDB delete payload: {e}"))
                })?;
                (del.filter, del.many)
            } else {
                (payload_val, false)
            };
            let filter_doc = json_to_doc(filter_val)?;
            let res = if many {
                coll.delete_many(filter_doc).await
            } else {
                coll.delete_one(filter_doc).await
            }
            .map_err(|e| ApsisError::Connector(format!("MongoDB delete failed: {e}")))?;

            json!({ "deleted_count": res.deleted_count })
        }
        other => {
            return Err(ApsisError::InvalidParams(format!(
                "Unsupported MongoDB op '{other}'. Expected one of: find, aggregate, insert, update, delete, list_collections"
            )));
        }
    };

    serde_json::to_string_pretty(&out)
        .map_err(|e| ApsisError::Connector(format!("Failed to serialize MongoDB output: {e}")))
}

fn parse_find_payload(payload: Option<Value>) -> Result<FindOptionsPayload> {
    let Some(val) = payload else {
        return Ok(FindOptionsPayload {
            filter: None,
            limit: None,
            skip: None,
            sort: None,
            projection: None,
        });
    };
    if let Value::Object(ref map) = val {
        let is_options_wrapper = map.contains_key("filter")
            || map.contains_key("limit")
            || map.contains_key("skip")
            || map.contains_key("sort")
            || map.contains_key("projection");
        if is_options_wrapper {
            return serde_json::from_value(val).map_err(|e| {
                ApsisError::InvalidParams(format!("Invalid MongoDB find options: {e}"))
            });
        }
    }
    Ok(FindOptionsPayload {
        filter: Some(val),
        limit: None,
        skip: None,
        sort: None,
        projection: None,
    })
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
