//! Library-wide error type.

use thiserror::Error;
pub type Result<T, E = AtlasError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum AtlasError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("migration error: {0}")]
    Migration(String),

    #[error("workspace already registered: {workspace_id}")]
    WorkspaceAlreadyRegistered { workspace_id: String },

    #[error("workspace not found: {workspace_id}")]
    WorkspaceNotFound { workspace_id: String },

    #[error("path is not inside the approved workspace root: {path}")]
    PathEscape { path: String },

    #[error("writer lease is held by another holder: {holder}")]
    WriterLeaseHeld { holder: String },

    #[error("writer lease expired (heartbeat stale by {stale_secs}s)")]
    WriterLeaseExpired { stale_secs: i64 },

    #[error("generation is not in the required state: {found}, required {required}")]
    GenerationStateInvalid { found: String, required: String },

    #[error(
        "active generation pointer mismatch: catalog has {catalog}, request bound to {request}"
    )]
    ActiveGenerationMismatch { catalog: String, request: String },

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("{0}")]
    Other(String),
}
