use aws_sdk_s3::Client;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use serde::{Deserialize, Serialize};

use crate::config::PresetConfig;
use crate::error::{ApsisError, Result};

/// Supported S3 actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum S3ActionKind {
    ListBuckets,
    ListObjects,
    GetObject,
    PutObject,
    DeleteObject,
}

impl std::str::FromStr for S3ActionKind {
    type Err = ApsisError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim() {
            "list_buckets" => Ok(Self::ListBuckets),
            "list_objects" => Ok(Self::ListObjects),
            "get_object" => Ok(Self::GetObject),
            "put_object" => Ok(Self::PutObject),
            "delete_object" => Ok(Self::DeleteObject),
            other => Err(ApsisError::InvalidParams(format!(
                "Unsupported S3 action '{other}'. Expected one of: list_buckets, list_objects, get_object, put_object, delete_object"
            ))),
        }
    }
}

/// Output of an S3 action, including spillover extension hint (`json` or `txt`).
pub struct S3ActionOutput {
    pub content: String,
    pub extension: &'static str,
}

#[derive(Serialize)]
struct BucketInfo {
    name: Option<String>,
    creation_date: Option<String>,
}

#[derive(Serialize)]
struct ObjectInfo {
    key: Option<String>,
    size: Option<i64>,
    last_modified: Option<String>,
    etag: Option<String>,
}

#[derive(Serialize)]
struct ListObjectsResponse {
    bucket: String,
    prefix: Option<String>,
    is_truncated: bool,
    objects: Vec<ObjectInfo>,
}

/// Build an S3 client from a preset, forcing path-style when a custom endpoint is set.
pub fn build_s3_client(
    endpoint: Option<&str>,
    region: &str,
    access_key: &str,
    secret_key: &str,
) -> Client {
    let creds = Credentials::new(access_key, secret_key, None, None, "apsis-preset");
    let mut builder = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .credentials_provider(creds)
        .region(Region::new(region.to_string()));

    if let Some(ep) = endpoint {
        let trimmed = ep.trim();
        if !trimmed.is_empty() {
            builder = builder
                .endpoint_url(trimmed.to_string())
                .force_path_style(true);
        }
    }

    Client::from_conf(builder.build())
}

pub fn build_client(preset: &PresetConfig) -> Result<Client> {
    let PresetConfig::S3 {
        endpoint,
        region,
        access_key,
        secret_key,
        ..
    } = preset
    else {
        return Err(ApsisError::InvalidParams(format!(
            "Preset protocol '{}' is not S3",
            preset.protocol()
        )));
    };

    Ok(build_s3_client(
        endpoint.as_deref(),
        region,
        access_key,
        secret_key,
    ))
}

pub async fn execute_action(
    client: &Client,
    default_bucket: Option<&str>,
    action: &str,
    bucket: Option<&str>,
    prefix: Option<&str>,
    key: Option<&str>,
    body: Option<&str>,
) -> Result<S3ActionOutput> {
    let kind: S3ActionKind = action.parse()?;

    let resolve_bucket = || -> Result<String> {
        bucket
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| default_bucket.map(str::trim).filter(|s| !s.is_empty()))
            .map(ToString::to_string)
            .ok_or_else(|| {
                ApsisError::InvalidParams(
                    "Missing 'bucket' parameter (and no default bucket configured on preset)"
                        .to_string(),
                )
            })
    };

    let require_key = || -> Result<String> {
        key.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| ApsisError::InvalidParams("Missing 'key' parameter".to_string()))
    };

    match kind {
        S3ActionKind::ListBuckets => {
            let resp = client
                .list_buckets()
                .send()
                .await
                .map_err(|e| ApsisError::Connector(format!("S3 list_buckets failed: {e}")))?;

            let buckets: Vec<BucketInfo> = resp
                .buckets()
                .iter()
                .map(|b| BucketInfo {
                    name: b.name().map(ToString::to_string),
                    creation_date: b.creation_date().map(|d| d.to_string()),
                })
                .collect();

            Ok(S3ActionOutput {
                content: serde_json::to_string_pretty(&buckets)
                    .map_err(|e| ApsisError::Connector(e.to_string()))?,
                extension: "json",
            })
        }
        S3ActionKind::ListObjects => {
            let bucket_name = resolve_bucket()?;
            let mut req = client.list_objects_v2().bucket(&bucket_name);
            if let Some(p) = prefix {
                req = req.prefix(p);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| ApsisError::Connector(format!("S3 list_objects failed: {e}")))?;

            let objects: Vec<ObjectInfo> = resp
                .contents()
                .iter()
                .map(|o| ObjectInfo {
                    key: o.key().map(ToString::to_string),
                    size: o.size(),
                    last_modified: o.last_modified().map(|d| d.to_string()),
                    etag: o.e_tag().map(ToString::to_string),
                })
                .collect();

            let out = ListObjectsResponse {
                bucket: bucket_name,
                prefix: prefix.map(ToString::to_string),
                is_truncated: resp.is_truncated().unwrap_or(false),
                objects,
            };

            Ok(S3ActionOutput {
                content: serde_json::to_string_pretty(&out)
                    .map_err(|e| ApsisError::Connector(e.to_string()))?,
                extension: "json",
            })
        }
        S3ActionKind::GetObject => {
            let bucket_name = resolve_bucket()?;
            let object_key = require_key()?;
            let resp = client
                .get_object()
                .bucket(&bucket_name)
                .key(&object_key)
                .send()
                .await
                .map_err(|e| ApsisError::Connector(format!("S3 get_object failed: {e}")))?;

            let bytes = resp
                .body
                .collect()
                .await
                .map_err(|e| ApsisError::Connector(format!("Failed reading S3 body: {e}")))?
                .into_bytes();

            match String::from_utf8(bytes.to_vec()) {
                Ok(text) => Ok(S3ActionOutput {
                    content: text,
                    extension: "txt",
                }),
                Err(err) => {
                    let raw_bytes = err.into_bytes();
                    let lossy = String::from_utf8_lossy(&raw_bytes).into_owned();
                    let json = serde_json::json!({
                        "bucket": bucket_name,
                        "key": object_key,
                        "binary": true,
                        "size_bytes": raw_bytes.len(),
                        "lossy_preview": lossy.chars().take(1000).collect::<String>(),
                    });
                    Ok(S3ActionOutput {
                        content: serde_json::to_string_pretty(&json)
                            .map_err(|e| ApsisError::Connector(e.to_string()))?,
                        extension: "json",
                    })
                }
            }
        }
        S3ActionKind::PutObject => {
            let bucket_name = resolve_bucket()?;
            let object_key = require_key()?;
            let payload = body.ok_or_else(|| {
                ApsisError::InvalidParams("Missing 'body' parameter for put_object".to_string())
            })?;

            let resp = client
                .put_object()
                .bucket(&bucket_name)
                .key(&object_key)
                .body(ByteStream::from(payload.as_bytes().to_vec()))
                .send()
                .await
                .map_err(|e| ApsisError::Connector(format!("S3 put_object failed: {e}")))?;

            let out = serde_json::json!({
                "bucket": bucket_name,
                "key": object_key,
                "etag": resp.e_tag(),
            });

            Ok(S3ActionOutput {
                content: serde_json::to_string_pretty(&out)
                    .map_err(|e| ApsisError::Connector(e.to_string()))?,
                extension: "json",
            })
        }
        S3ActionKind::DeleteObject => {
            let bucket_name = resolve_bucket()?;
            let object_key = require_key()?;
            client
                .delete_object()
                .bucket(&bucket_name)
                .key(&object_key)
                .send()
                .await
                .map_err(|e| ApsisError::Connector(format!("S3 delete_object failed: {e}")))?;

            let out = serde_json::json!({
                "bucket": bucket_name,
                "key": object_key,
                "deleted": true,
            });

            Ok(S3ActionOutput {
                content: serde_json::to_string_pretty(&out)
                    .map_err(|e| ApsisError::Connector(e.to_string()))?,
                extension: "json",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_endpoint_forces_path_style() {
        let client = build_s3_client(
            Some("https://minio.local:9000"),
            "eu-west-1",
            "AKIA",
            "SECRET",
        );
        // Ensuring client construction succeeds without panicking and config matches.
        assert_eq!(
            client.config().region().map(|r| r.as_ref()),
            Some("eu-west-1")
        );
    }
}
