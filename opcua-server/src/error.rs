//! Error types for the opcua-server crate.

use thiserror::Error;

/// Errors surfaced by the opcua-server crate.
#[derive(Error, Debug)]
pub enum ServerError {
    /// Underlying I/O or network error.
    #[error("IO error: {0}")]
    Io(String),

    /// Configuration deserialization or validation failure.
    #[error("Configuration error: {0}")]
    Config(String),

    /// Error originating from the open62541 backend.
    #[error("OPC UA backend error: {0}")]
    Backend(String),

    /// Client sent an invalid or malformed request.
    #[error("Invalid request: {0}")]
    BadRequest(String),

    /// Catch-all for unexpected errors.
    #[error("Other: {0}")]
    Other(String),
}
