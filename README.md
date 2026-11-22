<p align="center">
  <img src="assets/apsis.png" alt="Apsis logo" width="220" />
</p>

# Apsis

A single Rust binary combining a connection preset CLI manager (**PostgreSQL**, **MongoDB**, **S3**) and a **stdio MCP server** (`apsis mcp`).

The LLM has **no access** to any database or bucket until a preset is explicitly connected via the CLI, and passwords/secret keys are never exposed.

## Build & Install

```bash
cargo build --release
# Binary is produced at ./target/release/apsis
```

Or install locally:

```bash
cargo install --path .
```

## CLI Usage

```bash
# Add a preset (interactive prompts for missing fields, or pass flags)
apsis add dev-pg --protocol postgres --host 127.0.0.1 --port 5432 --user admin --password secret --database app
apsis add logs-s3 --protocol s3 --endpoint https://s3.example.com --region eu-west-1 --access-key AKIA... --secret-key secret --bucket logs

# List presets and connection status (secrets masked as ***)
apsis ls

# Expose or hide presets from the MCP server
apsis connect dev-pg
apsis disconnect dev-pg
apsis disconnect --all

# Remove a preset (also disconnects it if active)
apsis rm dev-pg

# Start the MCP server over stdin/stdout
apsis mcp
```

## Local Storage

Files live in `~/.config/apsis/` (override with `APSIS_CONFIG_DIR`):

- `presets.toml` (`0600` permissions): connection definitions.
- `state.json`: active presets exposed to the MCP server (`{ "active": ["dev-pg"] }`).

## MCP Server (`apsis mcp`)

`state.json` and `presets.toml` are reloaded on every tool call (while active clients/pools are cached in memory and evicted immediately upon disconnect or config change).

### Exposed Tools

1. `list_connections()` — lists active presets with `password: "***"`, or `"No active connections."`.
2. `db_query(preset, query, wait_ms?)` — executes raw SQL on PostgreSQL or a raw MongoDB command (`runCommand` passthrough):
   ```json
   {
     "database": "app",
     "command": { "find": "users", "filter": { "age": { "$gt": 18 } }, "limit": 10 }
   }
   ```
   `database` is optional and falls back to the preset's default. The `command` document is forwarded verbatim, so any operation the connected user's roles allow is expressible (`find`, `aggregate` with `"cursor": {}`, `insert`, `update`, `delete`, `listCollections`, `createIndexes`, `drop`, admin commands via `"database": "admin"`, …). Apsis adds no permission layer of its own — the database user's roles are the only gate. Cursor replies are drained with `getMore`, returning the full result set in `cursor.firstBatch`.
   If execution exceeds `wait_ms` (default **20s**, configurable via `APSIS_INLINE_WAIT_MS`), it returns `{ "status": "running", "job_id": "..." }` so MCP clients do not time out.
3. `job_status(job_id, wait_ms?, cancel?)` — polls (or long-polls up to `wait_ms`) a running `db_query` job, or cancels it with `cancel: true`.
4. `s3_action(preset, action, bucket?, prefix?, key?, body?)` — supports `list_buckets`, `list_objects`, `get_object`, `put_object`, and `delete_object` (forces path-style addressing when a custom `endpoint` is set).

### Large Output Spillover

Outputs larger than **50 KiB** (`51200` bytes, configurable via `APSIS_MAX_INLINE_BYTES`) are written to `/tmp/apsis-<preset>-<timestamp>.json` (or `.txt` for raw S3 text objects). The MCP response returns the file path, total byte size, and a 1000-character preview.
