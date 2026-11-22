use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn apsis_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apsis")
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("apsis-real-e2e-{tag}-{nanos}"))
}

struct McpClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn spawn(cfg_dir: &PathBuf, max_inline_bytes: Option<usize>) -> Self {
        let mut cmd = Command::new(apsis_bin());
        cmd.env("APSIS_CONFIG_DIR", cfg_dir)
            .args(["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(limit) = max_inline_bytes {
            cmd.env("APSIS_MAX_INLINE_BYTES", limit.to_string());
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut client = Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        };
        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "apsis-real-e2e", "version": "0.1.0" }
            }),
        );
        assert!(init["result"].is_object());
        client.notify("notifications/initialized", json!({}));
        client
    }

    fn notify(&mut self, method: &str, params: Value) {
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .unwrap();
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .unwrap();
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();

        let mut resp_line = String::new();
        let n = self.reader.read_line(&mut resp_line).unwrap();
        assert!(n > 0, "unexpected EOF from apsis mcp");
        let val: Value = serde_json::from_str(resp_line.trim()).unwrap();
        assert_eq!(val["id"], id);
        val
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let resp = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        (is_error, text)
    }

    fn close(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success());
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[test]
#[ignore = "requires live PostgreSQL, MongoDB, and S3Mock containers (run scripts/test-e2e-podman.sh)"]
fn real_postgres_mongodb_and_s3_e2e() {
    let cfg_dir = temp_dir("all");
    fs::create_dir_all(&cfg_dir).unwrap();

    let pg_host = env_or("APSIS_TEST_PG_HOST", "127.0.0.1");
    let pg_port = env_or("APSIS_TEST_PG_PORT", "15432");
    let pg_user = env_or("APSIS_TEST_PG_USER", "apsis");
    let pg_pass = env_or("APSIS_TEST_PG_PASSWORD", "apsis_pw");
    let pg_db = env_or("APSIS_TEST_PG_DB", "apsis_test");

    let mongo_host = env_or("APSIS_TEST_MONGO_HOST", "127.0.0.1");
    let mongo_port = env_or("APSIS_TEST_MONGO_PORT", "27117");
    let mongo_db = env_or("APSIS_TEST_MONGO_DB", "apsis_test");

    let s3_endpoint = env_or("APSIS_TEST_S3_ENDPOINT", "http://127.0.0.1:19090");
    let s3_region = env_or("APSIS_TEST_S3_REGION", "us-east-1");
    let s3_ak = env_or("APSIS_TEST_S3_ACCESS_KEY", "s3_access_key_test");
    let s3_sk = env_or("APSIS_TEST_S3_SECRET_KEY", "s3_secret_do_not_leak");
    let s3_bucket = env_or("APSIS_TEST_S3_BUCKET", "apsis-test");

    // Configure presets via CLI
    for args in [
        vec![
            "add",
            "real-pg",
            "--protocol",
            "postgres",
            "--host",
            &pg_host,
            "--port",
            &pg_port,
            "--user",
            &pg_user,
            "--password",
            &pg_pass,
            "--database",
            &pg_db,
        ],
        vec![
            "add",
            "real-mongo",
            "--protocol",
            "mongodb",
            "--host",
            &mongo_host,
            "--port",
            &mongo_port,
            "--database",
            &mongo_db,
        ],
        vec![
            "add",
            "real-s3",
            "--protocol",
            "s3",
            "--endpoint",
            &s3_endpoint,
            "--region",
            &s3_region,
            "--access-key",
            &s3_ak,
            "--secret-key",
            &s3_sk,
            "--bucket",
            &s3_bucket,
        ],
    ] {
        let out = Command::new(apsis_bin())
            .env("APSIS_CONFIG_DIR", &cfg_dir)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    for name in ["real-pg", "real-mongo", "real-s3"] {
        assert!(
            Command::new(apsis_bin())
                .env("APSIS_CONFIG_DIR", &cfg_dir)
                .args(["connect", name])
                .status()
                .unwrap()
                .success()
        );
    }

    // 1. Test PostgreSQL, MongoDB, S3 inline
    let mut mcp = McpClient::spawn(&cfg_dir, None);

    let (err, list_text) = mcp.call_tool("list_connections", json!({}));
    assert!(!err, "list_connections failed: {list_text}");
    assert!(list_text.contains("real-pg"));
    assert!(list_text.contains("real-mongo"));
    assert!(list_text.contains("real-s3"));
    assert!(!list_text.contains(&pg_pass));
    assert!(!list_text.contains(&s3_sk));

    // PostgreSQL DDL + DML + SELECT
    let (err, ddl) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-pg",
            "query": "CREATE TABLE IF NOT EXISTS items (id SERIAL PRIMARY KEY, name TEXT NOT NULL, meta JSONB, active BOOLEAN);"
        }),
    );
    assert!(!err, "pg create failed: {ddl}");

    let (err, ins) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-pg",
            "query": "INSERT INTO items (name, meta, active) VALUES ('alpha', '{\"k\": 1}'::jsonb, true), ('beta', '{\"k\": 2}'::jsonb, false)"
        }),
    );
    assert!(!err, "pg insert failed: {ins}");
    let ins_json: Value = serde_json::from_str(&ins).unwrap();
    assert_eq!(ins_json["rows_affected"], 2);

    let (err, sel) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-pg",
            "query": "SELECT id, name, meta, active FROM items ORDER BY id"
        }),
    );
    assert!(!err, "pg select failed: {sel}");
    let rows: Vec<Value> = serde_json::from_str(&sel).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "alpha");
    assert_eq!(rows[0]["meta"]["k"], 1);
    assert_eq!(rows[0]["active"], true);
    assert_eq!(rows[1]["name"], "beta");
    assert_eq!(rows[1]["active"], false);

    // MongoDB raw runCommand passthrough: insert, listCollections, find, aggregate, update, delete
    let (err, m_ins) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": {
                "database": mongo_db,
                "command": {
                    "insert": "orders",
                    "documents": [
                        { "item": "book", "qty": 2, "status": "paid" },
                        { "item": "pen", "qty": 5, "status": "paid" },
                        { "item": "bag", "qty": 1, "status": "pending" }
                    ]
                }
            }
        }),
    );
    assert!(!err, "mongo insert failed: {m_ins}");
    let m_ins_json: Value = serde_json::from_str(&m_ins).unwrap();
    assert_eq!(m_ins_json["n"], 3);

    let (err, m_colls) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": { "database": mongo_db, "command": { "listCollections": 1, "nameOnly": true } }
        }),
    );
    assert!(!err, "mongo listCollections failed: {m_colls}");
    let colls: Value = serde_json::from_str(&m_colls).unwrap();
    let names: Vec<&str> = colls["cursor"]["firstBatch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"orders"));

    let (err, m_find) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": {
                "database": mongo_db,
                "command": {
                    "find": "orders",
                    "filter": { "status": "paid" },
                    "sort": { "qty": -1 },
                    "limit": 1,
                    "projection": { "_id": 0, "item": 1, "qty": 1 }
                }
            }
        }),
    );
    assert!(!err, "mongo find failed: {m_find}");
    let found: Value = serde_json::from_str(&m_find).unwrap();
    assert_eq!(
        found["cursor"]["firstBatch"],
        json!([{ "item": "pen", "qty": 5 }])
    );

    let (err, m_agg) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": {
                "database": mongo_db,
                "command": {
                    "aggregate": "orders",
                    "pipeline": [
                        { "$match": { "status": "paid" } },
                        { "$group": { "_id": "$status", "total_qty": { "$sum": "$qty" } } }
                    ],
                    "cursor": {}
                }
            }
        }),
    );
    assert!(!err, "mongo aggregate failed: {m_agg}");
    let agg: Value = serde_json::from_str(&m_agg).unwrap();
    let agg_batch = agg["cursor"]["firstBatch"].as_array().unwrap();
    assert_eq!(agg_batch[0]["_id"], "paid");
    assert_eq!(agg_batch[0]["total_qty"], 7);

    let (err, m_upd) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": {
                "database": mongo_db,
                "command": {
                    "update": "orders",
                    "updates": [
                        { "q": { "item": "bag" }, "u": { "$set": { "status": "cancelled" } } }
                    ]
                }
            }
        }),
    );
    assert!(!err, "mongo update failed: {m_upd}");
    let upd: Value = serde_json::from_str(&m_upd).unwrap();
    assert_eq!(upd["nModified"], 1);

    let (err, m_del) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": {
                "database": mongo_db,
                "command": {
                    "delete": "orders",
                    "deletes": [ { "q": { "item": "bag" }, "limit": 1 } ]
                }
            }
        }),
    );
    assert!(!err, "mongo delete failed: {m_del}");
    let del: Value = serde_json::from_str(&m_del).unwrap();
    assert_eq!(del["n"], 1);

    // Cursor draining: batchSize 2 over 5 matching docs forces getMore rounds.
    let (err, m_bulk) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": {
                "database": mongo_db,
                "command": {
                    "insert": "orders",
                    "documents": [
                        { "item": "drain", "qty": 1 },
                        { "item": "drain", "qty": 2 },
                        { "item": "drain", "qty": 3 },
                        { "item": "drain", "qty": 4 },
                        { "item": "drain", "qty": 5 }
                    ]
                }
            }
        }),
    );
    assert!(!err, "mongo bulk insert failed: {m_bulk}");

    let (err, m_drain) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-mongo",
            "query": {
                "database": mongo_db,
                "command": {
                    "find": "orders",
                    "filter": { "item": "drain" },
                    "sort": { "qty": 1 },
                    "batchSize": 2,
                    "projection": { "_id": 0, "qty": 1 }
                }
            }
        }),
    );
    assert!(!err, "mongo drained find failed: {m_drain}");
    let drained: Value = serde_json::from_str(&m_drain).unwrap();
    assert_eq!(drained["cursor"]["id"], 0);
    assert_eq!(
        drained["cursor"]["firstBatch"],
        json!([{ "qty": 1 }, { "qty": 2 }, { "qty": 3 }, { "qty": 4 }, { "qty": 5 }])
    );

    // S3 list_buckets, put_object, list_objects, get_object, delete_object
    let (err, s3_buckets) = mcp.call_tool(
        "s3_action",
        json!({ "preset": "real-s3", "action": "list_buckets" }),
    );
    assert!(!err, "s3 list_buckets failed: {s3_buckets}");
    assert!(
        s3_buckets.contains(&s3_bucket),
        "expected bucket '{s3_bucket}' in list_buckets output: {s3_buckets}"
    );

    let (err, s3_put) = mcp.call_tool(
        "s3_action",
        json!({
            "preset": "real-s3",
            "action": "put_object",
            "key": "logs/hello.txt",
            "body": "hello from apsis"
        }),
    );
    assert!(!err, "s3 put_object failed: {s3_put}");

    let (err, s3_list) = mcp.call_tool(
        "s3_action",
        json!({
            "preset": "real-s3",
            "action": "list_objects",
            "prefix": "logs/"
        }),
    );
    assert!(!err, "s3 list_objects failed: {s3_list}");
    let list_json: Value = serde_json::from_str(&s3_list).unwrap();
    assert_eq!(list_json["objects"][0]["key"], "logs/hello.txt");

    let (err, s3_get) = mcp.call_tool(
        "s3_action",
        json!({
            "preset": "real-s3",
            "action": "get_object",
            "key": "logs/hello.txt"
        }),
    );
    assert!(!err, "s3 get_object failed: {s3_get}");
    assert_eq!(s3_get, "hello from apsis");

    let (err, s3_del) = mcp.call_tool(
        "s3_action",
        json!({
            "preset": "real-s3",
            "action": "delete_object",
            "key": "logs/hello.txt"
        }),
    );
    assert!(!err, "s3 delete_object failed: {s3_del}");

    // Slow query background job promotion & job_status polling against real PostgreSQL
    let (err, slow_resp) = mcp.call_tool(
        "db_query",
        json!({
            "preset": "real-pg",
            "query": "SELECT pg_sleep(0.3), 42 AS answer",
            "wait_ms": 50
        }),
    );
    assert!(!err, "slow pg query failed: {slow_resp}");
    let slow_json: Value = serde_json::from_str(&slow_resp).unwrap();
    assert_eq!(slow_json["status"], "running");
    let job_id = slow_json["job_id"].as_str().unwrap();

    let (err, polled_resp) = mcp.call_tool(
        "job_status",
        json!({
            "job_id": job_id,
            "wait_ms": 5000
        }),
    );
    assert!(!err, "job_status failed: {polled_resp}");
    let polled_rows: Vec<Value> = serde_json::from_str(&polled_resp).unwrap();
    assert_eq!(polled_rows[0]["answer"], 42);

    mcp.close();

    // 2. Test real spillover (.json for Postgres, .txt for S3 get_object)
    let mut mcp_spill = McpClient::spawn(&cfg_dir, Some(100));

    let (err, pg_spill) = mcp_spill.call_tool(
        "db_query",
        json!({
            "preset": "real-pg",
            "query": "SELECT repeat('x', 500) AS big_text"
        }),
    );
    assert!(!err, "pg spillover failed: {pg_spill}");
    let pg_spill_json: Value = serde_json::from_str(&pg_spill).unwrap();
    assert_eq!(pg_spill_json["spilled"], true);
    let pg_path = pg_spill_json["file_path"].as_str().unwrap();
    assert!(pg_path.ends_with(".json"));
    assert!(PathBuf::from(pg_path).exists());
    let _ = fs::remove_file(pg_path);

    let big_body = "A".repeat(1500);
    let (err, _) = mcp_spill.call_tool(
        "s3_action",
        json!({
            "preset": "real-s3",
            "action": "put_object",
            "key": "logs/big.txt",
            "body": big_body
        }),
    );
    assert!(!err);

    let (err, s3_spill) = mcp_spill.call_tool(
        "s3_action",
        json!({
            "preset": "real-s3",
            "action": "get_object",
            "key": "logs/big.txt"
        }),
    );
    assert!(!err, "s3 spillover failed: {s3_spill}");
    let s3_spill_json: Value = serde_json::from_str(&s3_spill).unwrap();
    assert_eq!(s3_spill_json["spilled"], true);
    assert_eq!(s3_spill_json["total_bytes"], 1500);
    assert_eq!(s3_spill_json["preview"].as_str().unwrap().len(), 1000);
    let s3_path = s3_spill_json["file_path"].as_str().unwrap();
    assert!(s3_path.ends_with(".txt"));
    assert_eq!(fs::read_to_string(s3_path).unwrap(), big_body);
    let _ = fs::remove_file(s3_path);

    mcp_spill.close();
    let _ = fs::remove_dir_all(&cfg_dir);
}
