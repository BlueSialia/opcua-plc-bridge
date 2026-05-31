//! Error types for the core-model crate.

use thiserror::Error;

/// Convenience `Result` type alias for core-model operations.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Central error type for the core-model crate.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A tag with the given id was not found in the registry.
    #[error("Tag not found: {0}")]
    TagNotFound(String),

    /// A tag definition with the given id was not found.
    #[error("Definition not found: {0}")]
    DefinitionNotFound(String),

    /// The runtime tag value does not match the expected data type.
    #[error("Type mismatch for tag {0}")]
    TypeMismatch(String),

    /// The provided configuration is invalid.
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    /// An unexpected internal error occurred.
    #[error("Internal error: {0}")]
    Internal(String),
}
