use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::config::{ConfigStore, PresetConfig};
use crate::connectors::{CachedClient, ConnectorCache, mongodb, postgres, s3};
use crate::error::ApsisError;
use crate::jobs::{JobManager, WaitOutcome};
use crate::spillover;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DbQueryParams {
    /// Name of the connected preset to query.
    pub preset: String,
    /// For PostgreSQL: raw SQL string. For MongoDB: JSON object { "database": "...", "collection": "...", "op": "find|aggregate|insert|update|delete|list_collections", "payload": ... }.
    pub query: Value,
    /// Optional inline wait duration in milliseconds before returning a background job_id (defaults to APSIS_INLINE_WAIT_MS or 20000ms).
    #[serde(default)]
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobStatusParams {
    /// Background job ID returned by db_query when a query exceeds the inline wait window.
    pub job_id: String,
    /// Optional long-poll wait duration in milliseconds (defaults to APSIS_INLINE_WAIT_MS or 20000ms; pass 0 for an immediate check).
    #[serde(default)]
    pub wait_ms: Option<u64>,
    /// Set to true to cancel the running background job.
    #[serde(default)]
    pub cancel: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct S3ActionParams {
    /// Name of the connected S3 preset.
    pub preset: String,
    /// Action to perform: list_buckets, list_objects, get_object, put_object, delete_object.
    pub action: String,
    /// Target bucket (optional if configured as default on the preset, or unused for list_buckets).
    #[serde(default)]
    pub bucket: Option<String>,
    /// Key prefix for list_objects.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Object key for get_object, put_object, or delete_object.
    #[serde(default)]
    pub key: Option<String>,
    /// UTF-8 body content for put_object.
    #[serde(default)]
    pub body: Option<String>,
}

/// Stateless MCP server reloading `state.json` and `presets.toml` on every tool call,
/// while pooling active clients and tracking slow background jobs in memory.
#[derive(Clone)]
pub struct ApsisMcpServer {
    store: ConfigStore,
    cache: Arc<ConnectorCache>,
    jobs: Arc<JobManager>,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl ApsisMcpServer {
    pub fn new(store: ConfigStore) -> Self {
        Self {
            store,
            cache: Arc::new(ConnectorCache::new()),
            jobs: Arc::new(JobManager::new()),
            tool_router: Self::tool_router(),
        }
    }

    async fn resolve_and_evict_if_inactive(
        &self,
        preset: &str,
    ) -> std::result::Result<PresetConfig, ApsisError> {
        match self.store.resolve_active_preset(preset) {
            Ok(cfg) => Ok(cfg),
            Err(err) => {
                self.cache.evict(preset).await;
                self.jobs.cancel_for_preset(preset).await;
                Err(err)
            }
        }
    }
}

#[tool_router]
impl ApsisMcpServer {
    /// List all currently connected presets (with passwords masked).
    #[tool(
        name = "list_connections",
        description = "List all currently connected database and S3 presets (passwords are always masked). Reloads state.json and presets.toml on each call."
    )]
    async fn list_connections(&self) -> Result<CallToolResult, ErrorData> {
        let active = match self.store.list_active_masked() {
            Ok(items) => items,
            Err(err) => return Ok(apsis_to_tool_error(err)),
        };

        let names: Vec<&str> = active.iter().map(|item| item.name.as_str()).collect();
        self.cache.retain_active(&names).await;

        if active.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "No active connections.",
            )]));
        }

        let json = serde_json::to_string_pretty(&active)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Execute a PostgreSQL SQL query or MongoDB JSON operation against a connected preset.
    #[tool(
        name = "db_query",
        description = "Execute a query against a connected PostgreSQL (raw SQL string) or MongoDB (JSON { database, collection, op, payload }) preset. If execution exceeds the inline wait window (20s by default), returns a job_id to poll via job_status. Large results spill over to /tmp/."
    )]
    async fn db_query(
        &self,
        Parameters(params): Parameters<DbQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let preset_cfg = match self.resolve_and_evict_if_inactive(&params.preset).await {
            Ok(cfg) => cfg,
            Err(err) => return Ok(apsis_to_tool_error(err)),
        };

        let wait = JobManager::resolve_wait_duration(params.wait_ms);
        let preset_name = params.preset.clone();

        let outcome = match &preset_cfg {
            PresetConfig::Postgres { .. } => {
                let Some(sql) = params.query.as_str().map(ToString::to_string) else {
                    return Ok(apsis_to_tool_error(ApsisError::InvalidParams(
                        "PostgreSQL db_query expects 'query' to be a SQL string".to_string(),
                    )));
                };
                let pool = match self.cache.get_or_init(&params.preset, &preset_cfg).await {
                    Ok(CachedClient::Postgres(pool)) => pool,
                    Ok(_) => unreachable!("preset protocol checked"),
                    Err(err) => return Ok(apsis_to_tool_error(err)),
                };
                self.jobs
                    .run_with_wait(preset_name.clone(), wait, async move {
                        let raw = postgres::execute_query(&pool, &sql)
                            .await
                            .map_err(|e| e.to_string())?;
                        spillover::format_mcp_output(&preset_name, &raw, "json")
                            .map_err(|e| e.to_string())
                    })
                    .await
            }
            PresetConfig::Mongodb { database, .. } => {
                let client = match self.cache.get_or_init(&params.preset, &preset_cfg).await {
                    Ok(CachedClient::Mongodb(client)) => client,
                    Ok(_) => unreachable!("preset protocol checked"),
                    Err(err) => return Ok(apsis_to_tool_error(err)),
                };
                let default_db = database.clone();
                let query = params.query;
                self.jobs
                    .run_with_wait(preset_name.clone(), wait, async move {
                        let raw = mongodb::execute_query(&client, default_db.as_deref(), &query)
                            .await
                            .map_err(|e| e.to_string())?;
                        spillover::format_mcp_output(&preset_name, &raw, "json")
                            .map_err(|e| e.to_string())
                    })
                    .await
            }
            PresetConfig::S3 { .. } => {
                return Ok(apsis_to_tool_error(ApsisError::InvalidParams(format!(
                    "Preset '{}' uses protocol 's3'; use s3_action instead",
                    params.preset
                ))));
            }
        };

        Ok(wait_outcome_to_tool_result(
            outcome,
            wait.as_millis() as u64,
        ))
    }

    /// Poll, long-poll, or cancel a slow background db_query job.
    #[tool(
        name = "job_status",
        description = "Check status of a background db_query job by job_id (long-polls up to wait_ms, 20s by default), or cancel it."
    )]
    async fn job_status(
        &self,
        Parameters(params): Parameters<JobStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let wait = JobManager::resolve_wait_duration(params.wait_ms);
        let cancel = params.cancel.unwrap_or(false);

        let Some((preset_name, outcome)) = self.jobs.poll(&params.job_id, wait, cancel).await
        else {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Job not found or expired: {}",
                params.job_id
            ))]));
        };

        if let Err(err) = self.resolve_and_evict_if_inactive(&preset_name).await {
            return Ok(apsis_to_tool_error(err));
        }

        Ok(wait_outcome_to_tool_result(
            outcome,
            wait.as_millis() as u64,
        ))
    }

    /// Execute an S3 action against a connected S3 preset.
    #[tool(
        name = "s3_action",
        description = "Execute an S3 action (list_buckets, list_objects, get_object, put_object, delete_object) on a connected S3 preset. Large results spill over to /tmp/."
    )]
    async fn s3_action(
        &self,
        Parameters(params): Parameters<S3ActionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let preset_cfg = match self.resolve_and_evict_if_inactive(&params.preset).await {
            Ok(cfg) => cfg,
            Err(err) => return Ok(apsis_to_tool_error(err)),
        };

        let PresetConfig::S3 {
            bucket: default_bucket,
            ..
        } = &preset_cfg
        else {
            return Ok(apsis_to_tool_error(ApsisError::InvalidParams(format!(
                "Preset protocol '{}' is not S3",
                preset_cfg.protocol()
            ))));
        };

        let client = match self.cache.get_or_init(&params.preset, &preset_cfg).await {
            Ok(CachedClient::S3(client)) => client,
            Ok(_) => unreachable!("preset protocol checked"),
            Err(err) => return Ok(apsis_to_tool_error(err)),
        };

        let result = match s3::execute_action(
            &client,
            default_bucket.as_deref(),
            &params.action,
            params.bucket.as_deref(),
            params.prefix.as_deref(),
            params.key.as_deref(),
            params.body.as_deref(),
        )
        .await
        {
            Ok(out) => out,
            Err(err) => return Ok(apsis_to_tool_error(err)),
        };

        match spillover::format_mcp_output(&params.preset, &result.content, result.extension) {
            Ok(formatted) => Ok(CallToolResult::success(vec![ContentBlock::text(formatted)])),
            Err(err) => Ok(apsis_to_tool_error(err)),
        }
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "apsis",
    instructions = "Use list_connections first to discover active presets, then db_query for PostgreSQL/MongoDB (poll with job_status if a job_id is returned) or s3_action for S3."
)]
impl ServerHandler for ApsisMcpServer {}

fn wait_outcome_to_tool_result(outcome: WaitOutcome, wait_ms: u64) -> CallToolResult {
    match outcome {
        WaitOutcome::Completed(output) => CallToolResult::success(vec![ContentBlock::text(output)]),
        WaitOutcome::Failed(err) => CallToolResult::error(vec![ContentBlock::text(err)]),
        WaitOutcome::Running { job_id } => {
            let payload = serde_json::json!({
                "status": "running",
                "job_id": job_id,
                "wait_ms": wait_ms,
                "message": "Query is still running in the background. Call job_status with this job_id to poll for completion."
            });
            CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&payload).unwrap_or(job_id),
            )])
        }
        WaitOutcome::Cancelled => {
            let payload = serde_json::json!({ "status": "cancelled" });
            CallToolResult::success(vec![ContentBlock::text(payload.to_string())])
        }
    }
}

fn apsis_to_tool_error(err: ApsisError) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(err.to_string())])
}

/// Start the stateless MCP server over stdin/stdout.
pub async fn serve_stdio(store: ConfigStore) -> anyhow::Result<()> {
    let server = ApsisMcpServer::new(store);
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("apsis-mcp-test-{tag}-{nanos}"))
    }

    fn extract_text(res: &CallToolResult) -> &str {
        res.content[0].as_text().unwrap().text.as_str()
    }

    #[tokio::test]
    async fn stateless_reload_and_not_connected_errors() {
        let dir = temp_dir("stateless");
        let store = ConfigStore::with_dir(&dir);
        store
            .add_preset(
                "dev-pg".to_string(),
                PresetConfig::Postgres {
                    host: "127.0.0.1".to_string(),
                    port: 5432,
                    user: "admin".to_string(),
                    password: "super-secret-password".to_string(),
                    database: "app".to_string(),
                },
                false,
            )
            .unwrap();

        let server = ApsisMcpServer::new(store.clone());

        // Initially no active connections
        let list1 = server.list_connections().await.unwrap();
        assert_eq!(extract_text(&list1), "No active connections.");

        // Disconnected preset returns explicit error
        let q1 = server
            .db_query(Parameters(DbQueryParams {
                preset: "dev-pg".to_string(),
                query: serde_json::json!("SELECT 1"),
                wait_ms: None,
            }))
            .await
            .unwrap();
        assert_eq!(q1.is_error, Some(true));
        assert!(extract_text(&q1).contains("Preset not connected"));

        // Connect preset externally -> server immediately sees it on next call
        store.connect_preset("dev-pg").unwrap();
        let list2 = server.list_connections().await.unwrap();
        let text2 = extract_text(&list2);
        assert!(text2.contains("dev-pg"));
        assert!(text2.contains("***"));
        assert!(!text2.contains("super-secret-password"));

        // Calling s3_action on a postgres preset returns explicit error
        let s3_err = server
            .s3_action(Parameters(S3ActionParams {
                preset: "dev-pg".to_string(),
                action: "list_buckets".to_string(),
                bucket: None,
                prefix: None,
                key: None,
                body: None,
            }))
            .await
            .unwrap();
        assert_eq!(s3_err.is_error, Some(true));
        assert!(extract_text(&s3_err).contains("not S3"));

        // Disconnect externally -> server immediately blocks access again
        store.disconnect_preset("dev-pg").unwrap();
        let list3 = server.list_connections().await.unwrap();
        assert_eq!(extract_text(&list3), "No active connections.");

        let _ = fs::remove_dir_all(&dir);
    }
}
