use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("apsis-e2e-{tag}-{nanos}"))
}

fn apsis_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apsis")
}

#[test]
fn cli_lifecycle_flags_and_interactive() {
    let cfg_dir = temp_dir("cli");
    fs::create_dir_all(&cfg_dir).unwrap();

    let out = Command::new(apsis_bin())
        .env("APSIS_CONFIG_DIR", &cfg_dir)
        .args([
            "add",
            "dev-pg",
            "--protocol",
            "postgres",
            "--host",
            "127.0.0.1",
            "--port",
            "5432",
            "--user",
            "admin",
            "--password",
            "top-secret-pg",
            "--database",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut child = Command::new(apsis_bin())
        .env("APSIS_CONFIG_DIR", &cfg_dir)
        .args(["add", "logs-s3"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "s3").unwrap();
        writeln!(stdin, "https://s3.example.com").unwrap();
        writeln!(stdin, "eu-west-1").unwrap();
        writeln!(stdin, "AKIA_TEST").unwrap();
        writeln!(stdin, "top-secret-s3").unwrap();
        writeln!(stdin, "logs").unwrap();
    }
    let interactive_out = child.wait_with_output().unwrap();
    assert!(
        interactive_out.status.success(),
        "interactive add failed: {}",
        String::from_utf8_lossy(&interactive_out.stderr)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(cfg_dir.join("presets.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let ls_out = Command::new(apsis_bin())
        .env("APSIS_CONFIG_DIR", &cfg_dir)
        .args(["ls"])
        .output()
        .unwrap();
    assert!(ls_out.status.success());
    let ls_stdout = String::from_utf8_lossy(&ls_out.stdout);
    assert!(ls_stdout.contains("dev-pg [disconnected]"));
    assert!(ls_stdout.contains("logs-s3 [disconnected]"));
    assert!(ls_stdout.contains("password=***"));
    assert!(!ls_stdout.contains("top-secret-pg"));
    assert!(!ls_stdout.contains("top-secret-s3"));

    for name in ["dev-pg", "logs-s3"] {
        let c = Command::new(apsis_bin())
            .env("APSIS_CONFIG_DIR", &cfg_dir)
            .args(["connect", name])
            .output()
            .unwrap();
        assert!(c.status.success());
    }

    let ls_conn = Command::new(apsis_bin())
        .env("APSIS_CONFIG_DIR", &cfg_dir)
        .args(["ls"])
        .output()
        .unwrap();
    let ls_conn_stdout = String::from_utf8_lossy(&ls_conn.stdout);
    assert!(ls_conn_stdout.contains("dev-pg [connected]"));
    assert!(ls_conn_stdout.contains("logs-s3 [connected]"));

    let disc_all = Command::new(apsis_bin())
        .env("APSIS_CONFIG_DIR", &cfg_dir)
        .args(["disconnect", "--all"])
        .output()
        .unwrap();
    assert!(disc_all.status.success());

    assert!(
        Command::new(apsis_bin())
            .env("APSIS_CONFIG_DIR", &cfg_dir)
            .args(["connect", "dev-pg"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(apsis_bin())
            .env("APSIS_CONFIG_DIR", &cfg_dir)
            .args(["rm", "dev-pg"])
            .status()
            .unwrap()
            .success()
    );

    let state_raw = fs::read_to_string(cfg_dir.join("state.json")).unwrap();
    let state_json: Value = serde_json::from_str(&state_raw).unwrap();
    assert_eq!(state_json["active"], json!([]));

    let _ = fs::remove_dir_all(&cfg_dir);
}

#[test]
fn mcp_stdio_roundtrip() {
    let cfg_dir = temp_dir("mcp-stdio");
    fs::create_dir_all(&cfg_dir).unwrap();

    assert!(
        Command::new(apsis_bin())
            .env("APSIS_CONFIG_DIR", &cfg_dir)
            .args([
                "add",
                "dev-pg",
                "--protocol",
                "postgres",
                "--host",
                "127.0.0.1",
                "--port",
                "5432",
                "--user",
                "admin",
                "--password",
                "never-leak-this",
                "--database",
                "app",
            ])
            .status()
            .unwrap()
            .success()
    );

    let mut mcp = Command::new(apsis_bin())
        .env("APSIS_CONFIG_DIR", &cfg_dir)
        .args(["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = mcp.stdin.take().unwrap();
    let stdout = mcp.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    let mut send_msg = |msg: Value| {
        let line = serde_json::to_string(&msg).unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    };

    let mut read_msg = || -> Value {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap();
        assert!(n > 0, "unexpected EOF from MCP server");
        serde_json::from_str(line.trim()).unwrap()
    };

    send_msg(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "apsis-test", "version": "0.1.0" }
        }
    }));
    let init_resp = read_msg();
    assert_eq!(init_resp["id"], 1);
    assert!(init_resp["result"].is_object());

    send_msg(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    send_msg(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools_resp = read_msg();
    let tools = tools_resp["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["db_query", "job_status", "list_connections", "s3_action"]
    );

    send_msg(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": "list_connections", "arguments": {} }
    }));
    let list_empty = read_msg();
    assert_eq!(
        list_empty["result"]["content"][0]["text"],
        "No active connections."
    );

    assert!(
        Command::new(apsis_bin())
            .env("APSIS_CONFIG_DIR", &cfg_dir)
            .args(["connect", "dev-pg"])
            .status()
            .unwrap()
            .success()
    );

    send_msg(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": { "name": "list_connections", "arguments": {} }
    }));
    let list_active = read_msg();
    let active_text = list_active["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(active_text.contains("dev-pg"));
    assert!(active_text.contains("***"));
    assert!(!active_text.contains("never-leak-this"));

    assert!(
        Command::new(apsis_bin())
            .env("APSIS_CONFIG_DIR", &cfg_dir)
            .args(["disconnect", "dev-pg"])
            .status()
            .unwrap()
            .success()
    );

    send_msg(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "db_query",
            "arguments": { "preset": "dev-pg", "query": "SELECT 1" }
        }
    }));
    let q_err = read_msg();
    assert_eq!(q_err["result"]["isError"], true);
    let err_text = q_err["result"]["content"][0]["text"].as_str().unwrap();
    assert!(err_text.contains("Preset not connected"));

    drop(stdin);
    let status = mcp.wait().unwrap();
    assert!(status.success());

    let _ = fs::remove_dir_all(&cfg_dir);
}
