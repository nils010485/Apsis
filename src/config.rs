use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ApsisError, Result};

pub const MASKED_SECRET: &str = "***";

/// Supported connection protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Postgres,
    Mongodb,
    S3,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Postgres => write!(f, "postgres"),
            Protocol::Mongodb => write!(f, "mongodb"),
            Protocol::S3 => write!(f, "s3"),
        }
    }
}

/// Configuration for a single connection preset stored in `presets.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "lowercase")]
pub enum PresetConfig {
    Postgres {
        host: String,
        #[serde(default = "default_postgres_port")]
        port: u16,
        user: String,
        password: String,
        database: String,
    },
    Mongodb {
        #[serde(default = "default_host")]
        host: String,
        #[serde(default = "default_mongo_port")]
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        database: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    S3 {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
        #[serde(default = "default_s3_region")]
        region: String,
        access_key: String,
        secret_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bucket: Option<String>,
    },
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_postgres_port() -> u16 {
    5432
}

fn default_mongo_port() -> u16 {
    27017
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

/// Public, secret-masked view of a preset exposed to CLI `ls` and MCP `list_connections`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskedPresetInfo {
    pub name: String,
    pub protocol: Protocol,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub password: String,
}

impl PresetConfig {
    pub fn protocol(&self) -> Protocol {
        match self {
            PresetConfig::Postgres { .. } => Protocol::Postgres,
            PresetConfig::Mongodb { .. } => Protocol::Mongodb,
            PresetConfig::S3 { .. } => Protocol::S3,
        }
    }

    /// Return a masked representation that never exposes passwords or secret keys.
    pub fn masked(&self, name: &str) -> MaskedPresetInfo {
        match self {
            PresetConfig::Postgres {
                host,
                port,
                user,
                database,
                ..
            } => MaskedPresetInfo {
                name: name.to_string(),
                protocol: Protocol::Postgres,
                host: Some(host.clone()),
                endpoint: None,
                port: Some(*port),
                user: Some(user.clone()),
                database: Some(database.clone()),
                bucket: None,
                region: None,
                password: MASKED_SECRET.to_string(),
            },
            PresetConfig::Mongodb {
                host,
                port,
                user,
                database,
                uri,
                ..
            } => MaskedPresetInfo {
                name: name.to_string(),
                protocol: Protocol::Mongodb,
                host: if uri.is_some() {
                    Some("<uri>".to_string())
                } else {
                    Some(host.clone())
                },
                endpoint: None,
                port: if uri.is_some() { None } else { Some(*port) },
                user: user.clone(),
                database: database.clone(),
                bucket: None,
                region: None,
                password: MASKED_SECRET.to_string(),
            },
            PresetConfig::S3 {
                endpoint,
                region,
                bucket,
                ..
            } => MaskedPresetInfo {
                name: name.to_string(),
                protocol: Protocol::S3,
                host: None,
                endpoint: endpoint.clone(),
                port: None,
                user: None,
                database: None,
                bucket: bucket.clone(),
                region: Some(region.clone()),
                password: MASKED_SECRET.to_string(),
            },
        }
    }
}

/// Contents of `presets.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetsFile {
    #[serde(default)]
    pub presets: BTreeMap<String, PresetConfig>,
}

/// Contents of `state.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub active: Vec<String>,
}

/// Resolves and manages `presets.toml` and `state.json` inside a config directory.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    dir: PathBuf,
}

impl ConfigStore {
    /// Create a store using `APSIS_CONFIG_DIR` if set, otherwise `~/.config/apsis/`.
    pub fn default_store() -> Result<Self> {
        if let Ok(custom) = std::env::var("APSIS_CONFIG_DIR") {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                return Ok(Self {
                    dir: PathBuf::from(trimmed),
                });
            }
        }
        let base = dirs::config_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .ok_or_else(|| {
                ApsisError::Config("Could not determine configuration directory".to_string())
            })?;
        Ok(Self {
            dir: base.join("apsis"),
        })
    }

    /// Create a store rooted at an explicit directory (primarily for tests).
    #[allow(dead_code)]
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn presets_path(&self) -> PathBuf {
        self.dir.join("presets.toml")
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    fn ensure_dir(&self) -> Result<()> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }

    pub fn load_presets(&self) -> Result<PresetsFile> {
        let path = self.presets_path();
        match fs::read_to_string(&path) {
            Ok(content) => {
                if content.trim().is_empty() {
                    return Ok(PresetsFile::default());
                }
                toml::from_str(&content).map_err(|e| {
                    ApsisError::Config(format!("Failed to parse {}: {e}", path.display()))
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PresetsFile::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_presets(&self, presets: &PresetsFile) -> Result<()> {
        self.ensure_dir()?;
        let path = self.presets_path();
        let serialized = toml::to_string_pretty(presets)
            .map_err(|e| ApsisError::Config(format!("Failed to serialize presets: {e}")))?;

        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        let mut file = opts.open(&path)?;
        file.write_all(serialized.as_bytes())?;
        file.flush()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn load_state(&self) -> Result<StateFile> {
        let path = self.state_path();
        match fs::read_to_string(&path) {
            Ok(content) => {
                if content.trim().is_empty() {
                    return Ok(StateFile::default());
                }
                serde_json::from_str(&content).map_err(|e| {
                    ApsisError::Config(format!("Failed to parse {}: {e}", path.display()))
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StateFile::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_state(&self, state: &StateFile) -> Result<()> {
        self.ensure_dir()?;
        let path = self.state_path();
        let serialized = serde_json::to_string_pretty(state)
            .map_err(|e| ApsisError::Config(format!("Failed to serialize state: {e}")))?;
        fs::write(&path, format!("{serialized}\n"))?;
        Ok(())
    }

    pub fn add_preset(&self, name: String, preset: PresetConfig, force: bool) -> Result<()> {
        validate_preset_name(&name)?;
        let mut presets = self.load_presets()?;
        if presets.presets.contains_key(&name) && !force {
            return Err(ApsisError::PresetAlreadyExists(name));
        }
        presets.presets.insert(name, preset);
        self.save_presets(&presets)
    }

    pub fn remove_preset(&self, name: &str) -> Result<bool> {
        let mut presets = self.load_presets()?;
        if presets.presets.remove(name).is_none() {
            return Err(ApsisError::PresetNotFound(name.to_string()));
        }
        self.save_presets(&presets)?;

        let mut state = self.load_state()?;
        let was_active = state.active.iter().any(|item| item == name);
        if was_active {
            state.active.retain(|item| item != name);
            self.save_state(&state)?;
        }
        Ok(was_active)
    }

    pub fn connect_preset(&self, name: &str) -> Result<bool> {
        let presets = self.load_presets()?;
        if !presets.presets.contains_key(name) {
            return Err(ApsisError::PresetNotFound(name.to_string()));
        }
        let mut state = self.load_state()?;
        if state.active.iter().any(|item| item == name) {
            return Ok(false);
        }
        state.active.push(name.to_string());
        self.save_state(&state)?;
        Ok(true)
    }

    pub fn disconnect_preset(&self, name: &str) -> Result<bool> {
        let mut state = self.load_state()?;
        let before = state.active.len();
        state.active.retain(|item| item != name);
        let removed = state.active.len() != before;
        if removed {
            self.save_state(&state)?;
        }
        Ok(removed)
    }

    pub fn disconnect_all(&self) -> Result<usize> {
        let mut state = self.load_state()?;
        let count = state.active.len();
        if count > 0 {
            state.active.clear();
            self.save_state(&state)?;
        }
        Ok(count)
    }

    /// Load both `state.json` and `presets.toml` and resolve an active preset.
    pub fn resolve_active_preset(&self, name: &str) -> Result<PresetConfig> {
        let state = self.load_state()?;
        if !state.active.iter().any(|item| item == name) {
            return Err(ApsisError::PresetNotConnected(name.to_string()));
        }
        let presets = self.load_presets()?;
        presets
            .presets
            .get(name)
            .cloned()
            .ok_or_else(|| ApsisError::PresetNotFound(name.to_string()))
    }

    /// Return masked metadata for all currently connected presets.
    pub fn list_active_masked(&self) -> Result<Vec<MaskedPresetInfo>> {
        let state = self.load_state()?;
        let presets = self.load_presets()?;
        let active_items = state
            .active
            .iter()
            .filter_map(|name| presets.presets.get(name).map(|cfg| cfg.masked(name)))
            .collect();
        Ok(active_items)
    }
}

fn validate_preset_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ApsisError::InvalidParams(
            "Preset name cannot be empty".to_string(),
        ));
    }
    if name
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(ApsisError::InvalidParams(
            "Preset name must contain only ASCII letters, digits, '-' or '_'".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_config_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("apsis-test-{tag}-{nanos}"))
    }

    #[test]
    fn roundtrip_presets_and_permissions() {
        let dir = temp_config_dir("perms");
        let store = ConfigStore::with_dir(&dir);

        store
            .add_preset(
                "dev-pg".to_string(),
                PresetConfig::Postgres {
                    host: "127.0.0.1".to_string(),
                    port: 5432,
                    user: "admin".to_string(),
                    password: "secret".to_string(),
                    database: "app".to_string(),
                },
                false,
            )
            .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.presets_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let loaded = store.load_presets().unwrap();
        assert!(loaded.presets.contains_key("dev-pg"));
        let masked = loaded.presets["dev-pg"].masked("dev-pg");
        assert_eq!(masked.password, "***");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn connect_disconnect_and_remove() {
        let dir = temp_config_dir("state");
        let store = ConfigStore::with_dir(&dir);

        store
            .add_preset(
                "logs-s3".to_string(),
                PresetConfig::S3 {
                    endpoint: Some("https://s3.example.com".to_string()),
                    region: "eu-west-1".to_string(),
                    access_key: "AKIA123".to_string(),
                    secret_key: "topsecret".to_string(),
                    bucket: Some("logs".to_string()),
                },
                false,
            )
            .unwrap();

        assert!(matches!(
            store.resolve_active_preset("logs-s3"),
            Err(ApsisError::PresetNotConnected(_))
        ));

        assert!(store.connect_preset("logs-s3").unwrap());
        assert!(!store.connect_preset("logs-s3").unwrap());
        assert!(store.resolve_active_preset("logs-s3").is_ok());

        let active = store.list_active_masked().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].password, "***");
        assert_eq!(active[0].bucket.as_deref(), Some("logs"));

        assert!(store.remove_preset("logs-s3").unwrap());
        assert!(store.load_state().unwrap().active.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_exact_spec_presets_toml_and_state_json() {
        let dir = temp_config_dir("spec");
        fs::create_dir_all(&dir).unwrap();

        let toml_src = r#"
[presets.dev-pg]
protocol = "postgres"
host = "127.0.0.1"
port = 5432
user = "admin"
password = "secret"
database = "app"

[presets.logs-s3]
protocol = "s3"
endpoint = "https://s3.example.com"
region = "eu-west-1"
access_key = "AKIA..."
secret_key = "secret"
bucket = "logs"
"#;
        let state_src = r#"{ "active": ["dev-pg", "logs-s3"] }"#;

        fs::write(dir.join("presets.toml"), toml_src).unwrap();
        fs::write(dir.join("state.json"), state_src).unwrap();

        let store = ConfigStore::with_dir(&dir);
        let active = store.list_active_masked().unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].name, "dev-pg");
        assert_eq!(active[0].password, "***");
        assert_eq!(active[1].name, "logs-s3");
        assert_eq!(active[1].password, "***");
        assert_eq!(active[1].bucket.as_deref(), Some("logs"));

        let _ = fs::remove_dir_all(&dir);
    }
}
