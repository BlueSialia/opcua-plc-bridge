use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

/// Central error type for the core-model crate.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Tag not found: {0}")]
    TagNotFound(String),

    #[error("Definition not found: {0}")]
    DefinitionNotFound(String),

    #[error("Type mismatch for tag {0}")]
    TypeMismatch(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Internal error: {0}")]
    Internal(String),
}
