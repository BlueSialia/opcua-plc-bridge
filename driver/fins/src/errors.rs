//! FINS driver error types.

use thiserror::Error;

/// Errors produced by the FINS driver.
#[derive(Debug, Error)]
pub enum DriverError {
    /// Underlying IO / network error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Operation timed out.
    #[error("Timeout")]
    Timeout,

    /// Protocol-level error or unexpected frame content.
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Mapping/configuration related error (e.g. unknown tag mapping).
    #[error("Mapping error: {0}")]
    Mapping(String),

    /// Miscellaneous other error.
    #[error("Other: {0}")]
    Other(String),
}

impl DriverError {
    /// Convenience constructor for protocol errors.
    pub fn protocol<S: Into<String>>(s: S) -> Self {
        DriverError::Protocol(s.into())
    }

    /// Convenience constructor for mapping errors.
    pub fn mapping<S: Into<String>>(s: S) -> Self {
        DriverError::Mapping(s.into())
    }

    /// Convenience constructor for other errors.
    pub fn other<S: Into<String>>(s: S) -> Self {
        DriverError::Other(s.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #feature DRV-FINS
    #[test]
    fn display_variants() {
        assert_eq!(format!("{}", DriverError::Timeout), "Timeout");
        assert_eq!(
            format!("{}", DriverError::protocol("bad frame")),
            "Protocol error: bad frame"
        );
        assert_eq!(
            format!("{}", DriverError::mapping("unknown tag")),
            "Mapping error: unknown tag"
        );
        assert_eq!(format!("{}", DriverError::other("boom")), "Other: boom");
    }

    /// #feature DRV-FINS
    #[test]
    fn from_io_error() {
        let io = std::io::Error::other("network down");
        let de: DriverError = io.into();
        assert!(matches!(de, DriverError::Io(_)));
        assert!(format!("{}", de).contains("IO error"));
    }
}
