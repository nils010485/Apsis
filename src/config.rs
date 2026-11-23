use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{ApsisError, Result};

pub const MASKED_SECRET: &str = "***";

const KEYRING_SERVICE: &str = "apsis";
const ENV_SECRET_STORE: &str = "APSIS_SECRET_STORE";
const KEYRING_UNAVAILABLE_HINT: &str = ". If no OS keyring is available (headless server, SSH, CI, container), set APSIS_SECRET_STORE=file to keep secrets in presets.toml";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetConfig {
    Postgres {
        host: String,
        port: u16,
        user: String,
        password: String,
        database: String,
    },
    Mongodb {
        host: String,
        port: u16,
        user: Option<String>,
        password: Option<String>,
        database: Option<String>,
        uri: Option<String>,
    },
    S3 {
        endpoint: Option<String>,
        region: String,
        access_key: String,
        secret_key: String,
        bucket: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "lowercase")]
enum PresetToml {
    Postgres {
        host: String,
        #[serde(default = "default_postgres_port")]
        port: u16,
        user: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bucket: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PresetsToml {
    #[serde(default)]
    presets: BTreeMap<String, PresetToml>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Secrets {
    password: Option<String>,
    secret_key: Option<String>,
    uri: Option<String>,
}

impl Secrets {
    fn iter(&self) -> impl Iterator<Item = (&'static str, Option<&str>)> {
        [
            ("password", self.password.as_deref()),
            ("secret_key", self.secret_key.as_deref()),
            ("uri", self.uri.as_deref()),
        ]
        .into_iter()
    }
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

    fn to_toml(&self) -> PresetToml {
        match self {
            PresetConfig::Postgres {
                host,
                port,
                user,
                password,
                database,
            } => PresetToml::Postgres {
                host: host.clone(),
                port: *port,
                user: user.clone(),
                password: Some(password.clone()),
                database: database.clone(),
            },
            PresetConfig::Mongodb {
                host,
                port,
                user,
                password,
                database,
                uri,
            } => PresetToml::Mongodb {
                host: host.clone(),
                port: *port,
                user: user.clone(),
                password: password.clone(),
                database: database.clone(),
                uri: uri.clone(),
            },
            PresetConfig::S3 {
                endpoint,
                region,
                access_key,
                secret_key,
                bucket,
            } => PresetToml::S3 {
                endpoint: endpoint.clone(),
                region: region.clone(),
                access_key: access_key.clone(),
                secret_key: Some(secret_key.clone()),
                bucket: bucket.clone(),
            },
        }
    }

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

impl PresetToml {
    fn strip_secrets(&mut self) -> Secrets {
        match self {
            PresetToml::Postgres { password, .. } => Secrets {
                password: password.take(),
                ..Secrets::default()
            },
            PresetToml::Mongodb { password, uri, .. } => Secrets {
                password: password.take(),
                uri: uri.take(),
                ..Secrets::default()
            },
            PresetToml::S3 { secret_key, .. } => Secrets {
                secret_key: secret_key.take(),
                ..Secrets::default()
            },
        }
    }

    fn hydrate(
        self,
        name: &str,
        mut lookup: impl FnMut(&str) -> Result<Option<String>>,
    ) -> Result<PresetConfig> {
        let mut resolve = |field: &str, inline: Option<String>| match inline {
            Some(value) => Ok(Some(value)),
            None => lookup(field),
        };
        let required = |field: &str, value: Option<String>| {
            value.ok_or_else(|| {
                ApsisError::Config(format!(
                    "Preset '{name}': secret '{field}' not found. Re-add the preset with `apsis add {name} --force` (secrets are read from the OS keyring unless APSIS_SECRET_STORE=file, and from presets.toml when stored inline)."
                ))
            })
        };
        match self {
            PresetToml::Postgres {
                host,
                port,
                user,
                password,
                database,
            } => Ok(PresetConfig::Postgres {
                host,
                port,
                user,
                password: required("password", resolve("password", password)?)?,
                database,
            }),
            PresetToml::Mongodb {
                host,
                port,
                user,
                password,
                database,
                uri,
            } => Ok(PresetConfig::Mongodb {
                host,
                port,
                user,
                password: resolve("password", password)?,
                database,
                uri: resolve("uri", uri)?,
            }),
            PresetToml::S3 {
                endpoint,
                region,
                access_key,
                secret_key,
                bucket,
            } => Ok(PresetConfig::S3 {
                endpoint,
                region,
                access_key,
                secret_key: required("secret_key", resolve("secret_key", secret_key)?)?,
                bucket,
            }),
        }
    }
}

trait SecretStore: Send + Sync {
    fn get(&self, preset: &str, field: &str) -> Result<Option<String>>;
    fn set(&self, preset: &str, field: &str, value: &str) -> Result<()>;
    fn delete(&self, preset: &str, field: &str) -> Result<()>;
}

struct OsKeyring;

impl SecretStore for OsKeyring {
    fn get(&self, preset: &str, field: &str) -> Result<Option<String>> {
        match entry(preset, field)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(keyring_error(preset, field, err)),
        }
    }

    fn set(&self, preset: &str, field: &str, value: &str) -> Result<()> {
        entry(preset, field)?
            .set_password(value)
            .map_err(|err| keyring_error(preset, field, err))
    }

    fn delete(&self, preset: &str, field: &str) -> Result<()> {
        match entry(preset, field)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(keyring_error(preset, field, err)),
        }
    }
}

fn entry(preset: &str, field: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, &format!("{preset}/{field}"))
        .map_err(|err| keyring_error(preset, field, err))
}

fn keyring_error(preset: &str, field: &str, err: keyring::Error) -> ApsisError {
    use keyring::Error;
    let hint = match err {
        Error::NoStorageAccess(_) | Error::PlatformFailure(_) | Error::NoDefaultStore => {
            KEYRING_UNAVAILABLE_HINT
        }
        _ => "",
    };
    ApsisError::Config(format!(
        "OS keyring error for '{KEYRING_SERVICE}: {preset}/{field}': {err}{hint}"
    ))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetsFile {
    pub presets: BTreeMap<String, PresetConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub active: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretsMode {
    Keyring,
    File,
}

impl SecretsMode {
    fn from_env() -> Result<Self> {
        Self::parse(std::env::var(ENV_SECRET_STORE).ok())
    }

    fn parse(raw: Option<String>) -> Result<Self> {
        let Some(raw) = raw else {
            return Ok(Self::Keyring);
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "keyring" => Ok(Self::Keyring),
            "file" => Ok(Self::File),
            other => Err(ApsisError::Config(format!(
                "Invalid {ENV_SECRET_STORE} value '{other}': expected 'keyring' or 'file'"
            ))),
        }
    }
}

#[derive(Clone)]
pub struct ConfigStore {
    dir: PathBuf,
    keyring: Option<Arc<dyn SecretStore>>,
}

impl ConfigStore {
    pub fn default_store() -> Result<Self> {
        let keyring = match SecretsMode::from_env()? {
            SecretsMode::Keyring => Some(Arc::new(OsKeyring) as Arc<dyn SecretStore>),
            SecretsMode::File => None,
        };
        if let Ok(custom) = std::env::var("APSIS_CONFIG_DIR") {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                return Ok(Self {
                    dir: PathBuf::from(trimmed),
                    keyring,
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
            keyring,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            keyring: None,
        }
    }

    #[cfg(test)]
    fn with_keyring(dir: impl Into<PathBuf>, keyring: Arc<dyn SecretStore>) -> Self {
        Self {
            dir: dir.into(),
            keyring: Some(keyring),
        }
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
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PresetsFile::default()),
            Err(e) => return Err(e.into()),
        };
        if content.trim().is_empty() {
            return Ok(PresetsFile::default());
        }
        let disk: PresetsToml = toml::from_str(&content)
            .map_err(|e| ApsisError::Config(format!("Failed to parse {}: {e}", path.display())))?;
        let mut presets = BTreeMap::new();
        for (name, entry) in disk.presets {
            let lookup = |field: &str| match self.keyring.as_deref() {
                Some(store) => store.get(&name, field),
                None => Ok(None),
            };
            let cfg = entry.hydrate(&name, lookup)?;
            presets.insert(name, cfg);
        }
        Ok(PresetsFile { presets })
    }

    pub fn save_presets(&self, presets: &PresetsFile) -> Result<()> {
        self.ensure_dir()?;
        let path = self.presets_path();
        let mut disk = PresetsToml::default();
        for (name, cfg) in &presets.presets {
            let mut entry = cfg.to_toml();
            if let Some(store) = self.keyring.as_deref() {
                let secrets = entry.strip_secrets();
                Self::store_secrets(store, name, &secrets)?;
            }
            disk.presets.insert(name.clone(), entry);
        }
        let serialized = toml::to_string_pretty(&disk)
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
        if let Some(store) = self.keyring.as_deref() {
            Self::store_secrets(store, name, &Secrets::default())?;
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

    fn store_secrets(store: &dyn SecretStore, preset: &str, secrets: &Secrets) -> Result<()> {
        for (field, value) in secrets.iter() {
            match value {
                Some(value) => store.set(preset, field, value)?,
                None => store.delete(preset, field)?,
            }
        }
        Ok(())
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
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_config_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("apsis-test-{tag}-{nanos}"))
    }

    struct MemoryKeyring {
        entries: Mutex<BTreeMap<(String, String), String>>,
        fail: bool,
    }

    impl MemoryKeyring {
        fn new(fail: bool) -> Self {
            Self {
                entries: Mutex::new(BTreeMap::new()),
                fail,
            }
        }

        fn check(&self) -> Result<()> {
            if self.fail {
                return Err(ApsisError::Config(
                    "OS keyring error for 'apsis: p/password': locked (simulated)".to_string(),
                ));
            }
            Ok(())
        }

        fn keys(&self) -> Vec<(String, String)> {
            self.entries.lock().unwrap().keys().cloned().collect()
        }

        fn clear(&self) {
            self.entries.lock().unwrap().clear();
        }
    }

    impl SecretStore for MemoryKeyring {
        fn get(&self, preset: &str, field: &str) -> Result<Option<String>> {
            self.check()?;
            Ok(self
                .entries
                .lock()
                .unwrap()
                .get(&(preset.to_string(), field.to_string()))
                .cloned())
        }

        fn set(&self, preset: &str, field: &str, value: &str) -> Result<()> {
            self.check()?;
            self.entries
                .lock()
                .unwrap()
                .insert((preset.to_string(), field.to_string()), value.to_string());
            Ok(())
        }

        fn delete(&self, preset: &str, field: &str) -> Result<()> {
            self.check()?;
            self.entries
                .lock()
                .unwrap()
                .remove(&(preset.to_string(), field.to_string()));
            Ok(())
        }
    }

    fn pg_cfg(password: &str) -> PresetConfig {
        PresetConfig::Postgres {
            host: "127.0.0.1".to_string(),
            port: 5432,
            user: "admin".to_string(),
            password: password.to_string(),
            database: "app".to_string(),
        }
    }

    fn s3_cfg(secret_key: &str) -> PresetConfig {
        PresetConfig::S3 {
            endpoint: Some("https://s3.example.com".to_string()),
            region: "eu-west-1".to_string(),
            access_key: "AKIA123".to_string(),
            secret_key: secret_key.to_string(),
            bucket: Some("logs".to_string()),
        }
    }

    fn mongo_uri_cfg(uri: &str) -> PresetConfig {
        PresetConfig::Mongodb {
            host: "127.0.0.1".to_string(),
            port: 27017,
            user: None,
            password: None,
            database: None,
            uri: Some(uri.to_string()),
        }
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
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

    #[test]
    fn secret_store_env_parsing() {
        assert_eq!(SecretsMode::parse(None).unwrap(), SecretsMode::Keyring);
        assert_eq!(
            SecretsMode::parse(Some(String::new())).unwrap(),
            SecretsMode::Keyring
        );
        assert_eq!(
            SecretsMode::parse(Some(" Keyring ".into())).unwrap(),
            SecretsMode::Keyring
        );
        assert_eq!(
            SecretsMode::parse(Some("file".into())).unwrap(),
            SecretsMode::File
        );
        let err = SecretsMode::parse(Some("vault".into()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("APSIS_SECRET_STORE") && err.contains("'keyring' or 'file'"),
            "{err}"
        );
    }

    #[test]
    fn keyring_error_hint_only_for_storage_failures() {
        let locked = keyring_error(
            "p",
            "password",
            keyring::Error::NoStorageAccess("locked".into()),
        );
        let msg = locked.to_string();
        assert!(
            msg.contains("apsis: p/password") && msg.contains("locked"),
            "{msg}"
        );
        assert!(msg.contains("APSIS_SECRET_STORE=file"), "{msg}");
        assert!(
            keyring_error("p", "password", keyring::Error::NoDefaultStore)
                .to_string()
                .contains("APSIS_SECRET_STORE=file")
        );
        let too_long =
            keyring_error("p", "password", keyring::Error::TooLong("user".into(), 100)).to_string();
        assert!(
            too_long.contains("apsis: p/password") && !too_long.contains("APSIS_SECRET_STORE"),
            "{too_long}"
        );
    }

    #[test]
    fn keyring_strips_secrets_from_toml_and_hydrates() {
        let dir = temp_config_dir("keyring");
        let keyring = Arc::new(MemoryKeyring::new(false));
        let store = ConfigStore::with_keyring(&dir, keyring.clone());
        store
            .add_preset("dev-pg".into(), pg_cfg("pg-secret-123"), false)
            .unwrap();
        store
            .add_preset("logs-s3".into(), s3_cfg("s3-secret-456"), false)
            .unwrap();
        store
            .add_preset(
                "mongo".into(),
                mongo_uri_cfg("mongodb://u:mongo-secret-789@h/db"),
                false,
            )
            .unwrap();

        let toml_src = fs::read_to_string(store.presets_path()).unwrap();
        for secret in [
            "pg-secret-123",
            "s3-secret-456",
            "mongo-secret-789",
            "password",
            "secret_key",
            "uri",
        ] {
            assert!(!toml_src.contains(secret), "{toml_src}");
        }
        assert!(
            toml_src.contains("[presets.dev-pg]") && toml_src.contains("user = \"admin\""),
            "{toml_src}"
        );
        assert_eq!(
            keyring.keys(),
            pairs(&[
                ("dev-pg", "password"),
                ("logs-s3", "secret_key"),
                ("mongo", "uri")
            ])
        );

        let loaded = store.load_presets().unwrap();
        assert_eq!(loaded.presets["dev-pg"], pg_cfg("pg-secret-123"));
        assert_eq!(loaded.presets["logs-s3"], s3_cfg("s3-secret-456"));
        assert_eq!(
            loaded.presets["mongo"],
            mongo_uri_cfg("mongodb://u:mongo-secret-789@h/db")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyring_legacy_inline_preset_loads_then_migrates_on_save() {
        let dir = temp_config_dir("legacy");
        fs::create_dir_all(&dir).unwrap();
        let keyring = Arc::new(MemoryKeyring::new(false));
        let store = ConfigStore::with_keyring(&dir, keyring.clone());
        fs::write(
            dir.join("presets.toml"),
            "[presets.old-pg]\nprotocol = \"postgres\"\nhost = \"127.0.0.1\"\nport = 5432\nuser = \"admin\"\npassword = \"legacy-secret\"\ndatabase = \"app\"\n",
        )
        .unwrap();

        let loaded = store.load_presets().unwrap();
        assert_eq!(loaded.presets["old-pg"], pg_cfg("legacy-secret"));
        assert!(keyring.keys().is_empty());

        store
            .add_preset("new-s3".into(), s3_cfg("new-secret"), false)
            .unwrap();
        assert_eq!(
            keyring.keys(),
            pairs(&[("new-s3", "secret_key"), ("old-pg", "password")])
        );
        let toml_src = fs::read_to_string(store.presets_path()).unwrap();
        assert!(
            !toml_src.contains("legacy-secret") && !toml_src.contains("new-secret"),
            "{toml_src}"
        );
        let loaded = store.load_presets().unwrap();
        assert_eq!(loaded.presets["old-pg"], pg_cfg("legacy-secret"));
        assert_eq!(loaded.presets["new-s3"], s3_cfg("new-secret"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyring_force_readd_and_remove_update_entries() {
        let dir = temp_config_dir("keyring-rm");
        let keyring = Arc::new(MemoryKeyring::new(false));
        let store = ConfigStore::with_keyring(&dir, keyring.clone());
        store.add_preset("p".into(), pg_cfg("one"), false).unwrap();
        assert_eq!(keyring.keys(), pairs(&[("p", "password")]));

        store.add_preset("p".into(), s3_cfg("two"), true).unwrap();
        assert_eq!(keyring.keys(), pairs(&[("p", "secret_key")]));
        assert_eq!(store.load_presets().unwrap().presets["p"], s3_cfg("two"));

        assert!(!store.remove_preset("p").unwrap());
        assert!(keyring.keys().is_empty());
        assert!(store.load_presets().unwrap().presets.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyring_missing_required_secret_errors_with_remediation() {
        let dir = temp_config_dir("missing");
        let keyring = Arc::new(MemoryKeyring::new(false));
        let store = ConfigStore::with_keyring(&dir, keyring.clone());
        store.add_preset("p".into(), pg_cfg("one"), false).unwrap();
        keyring.clear();

        let err = store.load_presets().unwrap_err().to_string();
        assert!(
            err.contains("Preset 'p': secret 'password' not found"),
            "{err}"
        );
        assert!(
            err.contains("apsis add p --force") && err.contains("APSIS_SECRET_STORE=file"),
            "{err}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyring_failure_leaves_presets_untouched() {
        let dir = temp_config_dir("failing");
        let keyring = Arc::new(MemoryKeyring::new(true));
        let store = ConfigStore::with_keyring(&dir, keyring.clone());

        let err = store
            .add_preset("p".into(), pg_cfg("one"), false)
            .unwrap_err();
        assert!(err.to_string().contains("simulated"), "{err}");
        assert!(!store.presets_path().exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
