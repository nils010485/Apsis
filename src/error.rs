use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApsisError {
    #[error("Preset not connected: {0}")]
    PresetNotConnected(String),

    #[error("Preset not found: {0}")]
    PresetNotFound(String),

    #[error("Preset already exists: {0}")]
    PresetAlreadyExists(String),

    #[error("Invalid parameters: {0}")]
    InvalidParams(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Connector error: {0}")]
    Connector(String),
}

pub type Result<T> = std::result::Result<T, ApsisError>;
