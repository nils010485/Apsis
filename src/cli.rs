use std::io::{self, BufRead, IsTerminal, Write};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::{ConfigStore, MASKED_SECRET, PresetConfig, Protocol};
use crate::error::{ApsisError, Result};
use crate::mcp;

#[derive(Debug, Parser)]
#[command(
    name = "apsis",
    version,
    about = "CLI preset manager and stateless MCP server for PostgreSQL, MongoDB, and S3"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Add a new connection preset (via flags or interactive prompt)
    Add(Box<AddArgs>),
    /// Remove a preset (and disconnect it if active)
    Rm {
        /// Preset name
        name: String,
    },
    /// List all presets with connection status and masked secrets
    Ls,
    /// Connect a preset (exposes it to the MCP server)
    Connect {
        /// Preset name
        name: String,
    },
    /// Disconnect a preset (or all presets with --all)
    Disconnect(DisconnectArgs),
    /// Run the MCP server on stdin/stdout
    Mcp,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliProtocol {
    Postgres,
    Mongodb,
    S3,
}

impl From<CliProtocol> for Protocol {
    fn from(value: CliProtocol) -> Self {
        match value {
            CliProtocol::Postgres => Protocol::Postgres,
            CliProtocol::Mongodb => Protocol::Mongodb,
            CliProtocol::S3 => Protocol::S3,
        }
    }
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Preset name
    pub name: String,
    /// Protocol (postgres, mongodb, s3)
    #[arg(short, long, value_enum)]
    pub protocol: Option<CliProtocol>,
    /// Hostname (postgres, mongodb)
    #[arg(long)]
    pub host: Option<String>,
    /// Port (postgres: 5432, mongodb: 27017)
    #[arg(long)]
    pub port: Option<u16>,
    /// Username (postgres, mongodb)
    #[arg(long)]
    pub user: Option<String>,
    /// Password (postgres, mongodb)
    #[arg(long)]
    pub password: Option<String>,
    /// Database name (postgres, mongodb)
    #[arg(long)]
    pub database: Option<String>,
    /// MongoDB connection URI (optional alternative to host/port)
    #[arg(long)]
    pub uri: Option<String>,
    /// Custom S3 endpoint URL (forces path-style addressing)
    #[arg(long)]
    pub endpoint: Option<String>,
    /// S3 region
    #[arg(long)]
    pub region: Option<String>,
    /// S3 access key ID
    #[arg(long)]
    pub access_key: Option<String>,
    /// S3 secret access key
    #[arg(long)]
    pub secret_key: Option<String>,
    /// Read the secret (postgres/mongodb password or S3 secret key) from the first line of stdin instead of prompting
    #[arg(
        long,
        conflicts_with("password"),
        conflicts_with("secret_key"),
        conflicts_with("uri")
    )]
    pub secret_stdin: bool,
    /// Optional default S3 bucket
    #[arg(long)]
    pub bucket: Option<String>,
    /// Overwrite preset if it already exists
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct DisconnectArgs {
    /// Preset name to disconnect
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub name: Option<String>,
    /// Disconnect all active presets
    #[arg(long)]
    pub all: bool,
}

pub async fn run(cli: Cli, store: ConfigStore) -> anyhow::Result<()> {
    match cli.command {
        Commands::Add(args) => handle_add(*args, &store)?,
        Commands::Rm { name } => {
            let was_active = store.remove_preset(&name)?;
            if was_active {
                println!("Removed and disconnected preset '{name}'.");
            } else {
                println!("Removed preset '{name}'.");
            }
        }
        Commands::Ls => handle_ls(&store)?,
        Commands::Connect { name } => {
            let added = store.connect_preset(&name)?;
            if added {
                println!("Connected preset '{name}'.");
            } else {
                println!("Preset '{name}' is already connected.");
            }
        }
        Commands::Disconnect(args) => {
            if args.all {
                let count = store.disconnect_all()?;
                println!("Disconnected {count} preset(s).");
            } else if let Some(name) = args.name {
                let removed = store.disconnect_preset(&name)?;
                if removed {
                    println!("Disconnected preset '{name}'.");
                } else {
                    println!("Preset '{name}' was not connected.");
                }
            }
        }
        Commands::Mcp => {
            mcp::serve_stdio(store).await?;
        }
    }
    Ok(())
}

fn handle_ls(store: &ConfigStore) -> Result<()> {
    let presets = store.load_presets()?;
    let state = store.load_state()?;

    if presets.presets.is_empty() {
        println!("No presets configured.");
        return Ok(());
    }

    for (name, cfg) in &presets.presets {
        let status = if state.active.iter().any(|a| a == name) {
            "connected"
        } else {
            "disconnected"
        };
        let masked = cfg.masked(name);
        let mut details = Vec::new();
        if let Some(h) = masked.host {
            details.push(format!("host={h}"));
        }
        if let Some(ep) = masked.endpoint {
            details.push(format!("endpoint={ep}"));
        }
        if let Some(p) = masked.port {
            details.push(format!("port={p}"));
        }
        if let Some(u) = masked.user {
            details.push(format!("user={u}"));
        }
        if let Some(d) = masked.database {
            details.push(format!("database={d}"));
        }
        if let Some(r) = masked.region {
            details.push(format!("region={r}"));
        }
        if let Some(b) = masked.bucket {
            details.push(format!("bucket={b}"));
        }
        details.push(format!("password={MASKED_SECRET}"));

        println!(
            "{name} [{status}] ({}) {}",
            masked.protocol,
            details.join(" ")
        );
    }
    Ok(())
}

fn handle_add(args: AddArgs, store: &ConfigStore) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout();

    let stdin_secret = if args.secret_stdin {
        Some(read_stdin_secret(&mut reader)?)
    } else {
        None
    };

    let protocol = match args.protocol {
        Some(p) => Protocol::from(p),
        None => loop {
            let raw = prompt_line(
                &mut reader,
                &mut stdout,
                "Protocol (postgres/mongodb/s3)",
                None,
            )?;
            match raw.trim().to_ascii_lowercase().as_str() {
                "postgres" | "pg" => break Protocol::Postgres,
                "mongodb" | "mongo" => break Protocol::Mongodb,
                "s3" => break Protocol::S3,
                other => {
                    writeln!(
                        stdout,
                        "Unsupported protocol '{other}'. Choose postgres, mongodb, or s3."
                    )?;
                }
            }
        },
    };

    let preset = match protocol {
        Protocol::Postgres => {
            let host =
                resolve_with_default(args.host, &mut reader, &mut stdout, "Host", "127.0.0.1")?;
            let port = resolve_port(args.port, &mut reader, &mut stdout, 5432)?;
            let user = resolve_required(args.user, &mut reader, &mut stdout, "User")?;
            let password = resolve_secret(
                args.password,
                "--password",
                stdin_secret,
                &mut reader,
                &mut stdout,
                "Password",
            )?;
            let database = resolve_required(args.database, &mut reader, &mut stdout, "Database")?;
            PresetConfig::Postgres {
                host,
                port,
                user,
                password,
                database,
            }
        }
        Protocol::Mongodb => {
            if let Some(uri) = args.uri.filter(|s| !s.trim().is_empty()) {
                warn_secret_flag("--uri");
                PresetConfig::Mongodb {
                    host: args.host.unwrap_or_else(|| "127.0.0.1".to_string()),
                    port: args.port.unwrap_or(27017),
                    user: args.user,
                    password: args.password,
                    database: args.database,
                    uri: Some(uri),
                }
            } else {
                let host =
                    resolve_with_default(args.host, &mut reader, &mut stdout, "Host", "127.0.0.1")?;
                let port = resolve_port(args.port, &mut reader, &mut stdout, 27017)?;
                let user =
                    resolve_optional(args.user, &mut reader, &mut stdout, "User (optional)")?;
                let password = if let Some(v) = stdin_secret {
                    Some(v)
                } else if user.is_some() {
                    resolve_optional_secret(
                        args.password,
                        "--password",
                        &mut reader,
                        &mut stdout,
                        "Password (optional)",
                    )?
                } else {
                    args.password.inspect(|_| warn_secret_flag("--password"))
                };
                let database = resolve_optional(
                    args.database,
                    &mut reader,
                    &mut stdout,
                    "Database (optional)",
                )?;
                PresetConfig::Mongodb {
                    host,
                    port,
                    user,
                    password,
                    database,
                    uri: None,
                }
            }
        }
        Protocol::S3 => {
            let endpoint = resolve_optional(
                args.endpoint,
                &mut reader,
                &mut stdout,
                "Endpoint URL (optional, leave empty for AWS S3)",
            )?;
            let region =
                resolve_with_default(args.region, &mut reader, &mut stdout, "Region", "us-east-1")?;
            let access_key =
                resolve_required(args.access_key, &mut reader, &mut stdout, "Access key")?;
            let secret_key = resolve_secret(
                args.secret_key,
                "--secret-key",
                stdin_secret,
                &mut reader,
                &mut stdout,
                "Secret key",
            )?;
            let bucket = resolve_optional(
                args.bucket,
                &mut reader,
                &mut stdout,
                "Default bucket (optional)",
            )?;
            PresetConfig::S3 {
                endpoint,
                region,
                access_key,
                secret_key,
                bucket,
            }
        }
    };

    let name = args.name;
    store.add_preset(name.clone(), preset, args.force)?;
    println!(
        "Saved preset '{name}' to {}.",
        store.presets_path().display()
    );
    Ok(())
}

fn prompt_line<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    match default {
        Some(d) => write!(writer, "{label} [{d}]: ")?,
        None => write!(writer, "{label}: ")?,
    }
    writer.flush()?;
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 && default.is_none() {
        return Err(ApsisError::InvalidParams(format!(
            "Unexpected EOF while reading '{label}'"
        )));
    }
    let trimmed = line.trim().to_string();
    if trimmed.is_empty()
        && let Some(d) = default
    {
        return Ok(d.to_string());
    }
    Ok(trimmed)
}

fn resolve_with_default<R: BufRead, W: Write>(
    flag: Option<String>,
    reader: &mut R,
    writer: &mut W,
    label: &str,
    default: &str,
) -> Result<String> {
    match flag {
        Some(v) => Ok(v),
        None => prompt_line(reader, writer, label, Some(default)),
    }
}

fn resolve_required<R: BufRead, W: Write>(
    flag: Option<String>,
    reader: &mut R,
    writer: &mut W,
    label: &str,
) -> Result<String> {
    match flag {
        Some(v) => Ok(v),
        None => loop {
            let v = prompt_line(reader, writer, label, None)?;
            if !v.is_empty() {
                return Ok(v);
            }
            writeln!(writer, "{label} is required.")?;
        },
    }
}

fn resolve_optional<R: BufRead, W: Write>(
    flag: Option<String>,
    reader: &mut R,
    writer: &mut W,
    label: &str,
) -> Result<Option<String>> {
    match flag {
        Some(v) => {
            let t = v.trim().to_string();
            Ok((!t.is_empty()).then_some(t))
        }
        None => {
            let v = prompt_line(reader, writer, label, Some(""))?;
            Ok((!v.is_empty()).then_some(v))
        }
    }
}

fn resolve_port<R: BufRead, W: Write>(
    flag: Option<u16>,
    reader: &mut R,
    writer: &mut W,
    default: u16,
) -> Result<u16> {
    if let Some(p) = flag {
        return Ok(p);
    }
    let default_str = default.to_string();
    loop {
        let raw = prompt_line(reader, writer, "Port", Some(&default_str))?;
        match raw.parse::<u16>() {
            Ok(p) => return Ok(p),
            Err(_) => writeln!(writer, "Invalid port '{raw}'.")?,
        }
    }
}

fn read_secret_line<R: BufRead>(reader: &mut R, label: &str) -> Result<String> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(ApsisError::InvalidParams(format!(
            "Unexpected EOF while reading '{label}'"
        )));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn read_stdin_secret<R: BufRead>(reader: &mut R) -> Result<String> {
    let secret = read_secret_line(reader, "the secret from stdin")?;
    if secret.is_empty() {
        return Err(ApsisError::InvalidParams(
            "The secret read from stdin is empty".to_string(),
        ));
    }
    Ok(secret)
}

fn warn_secret_flag(flag: &str) {
    eprintln!(
        "Warning: a secret passed with {flag} is exposed in the shell history and process list; prefer the prompt or --secret-stdin."
    );
}

fn prompt_secret<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    label: &str,
) -> Result<String> {
    if io::stdin().is_terminal() {
        return Ok(rpassword::prompt_password(format!("{label}: "))?);
    }
    write!(writer, "{label}: ")?;
    writer.flush()?;
    read_secret_line(reader, label)
}

fn resolve_secret<R: BufRead, W: Write>(
    flag: Option<String>,
    flag_name: &str,
    stdin_secret: Option<String>,
    reader: &mut R,
    writer: &mut W,
    label: &str,
) -> Result<String> {
    if let Some(v) = flag {
        warn_secret_flag(flag_name);
        return Ok(v);
    }
    if let Some(v) = stdin_secret {
        return Ok(v);
    }
    loop {
        let v = prompt_secret(reader, writer, label)?;
        if !v.is_empty() {
            return Ok(v);
        }
        writeln!(writer, "{label} is required.")?;
    }
}

fn resolve_optional_secret<R: BufRead, W: Write>(
    flag: Option<String>,
    flag_name: &str,
    reader: &mut R,
    writer: &mut W,
    label: &str,
) -> Result<Option<String>> {
    if let Some(v) = flag {
        warn_secret_flag(flag_name);
        return Ok((!v.is_empty()).then_some(v));
    }
    let v = prompt_secret(reader, writer, label)?;
    Ok((!v.is_empty()).then_some(v))
}
