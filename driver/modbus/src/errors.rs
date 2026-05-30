//! Error types for the Modbus driver.

use thiserror::Error;

/// Errors produced by the Modbus driver.
#[derive(Debug, Error)]
pub enum DriverError {
    /// Underlying IO / network error.
    #[error("IO/network error: {0}")]
    Io(#[from] std::io::Error),

    /// Modbus protocol error / exception.
    /// Contains a human-friendly description or the original exception message.
    #[error("Modbus protocol error: {0}")]
    Modbus(String),

    /// Operation timed out.
    #[error("Timeout")]
    Timeout,

    /// Mapping/configuration related error (e.g. unknown tag mapping).
    #[error("Mapping error: {0}")]
    Mapping(String),

    /// Miscellaneous other error.
    #[error("Other: {0}")]
    Other(String),
}

impl DriverError {
    /// Convenience constructor for `Modbus` errors.
    pub fn modbus<S: Into<String>>(s: S) -> Self {
        DriverError::Modbus(s.into())
    }

    /// Convenience constructor for `Mapping` errors.
    pub fn mapping<S: Into<String>>(s: S) -> Self {
        DriverError::Mapping(s.into())
    }

    /// Convenience constructor for `Other` errors.
    pub fn other<S: Into<String>>(s: S) -> Self {
        DriverError::Other(s.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #feature DRV-MODBUS
    #[test]
    fn display_variants() {
        let e = DriverError::Timeout;
        assert_eq!(format!("{}", e), "Timeout");

        let e = DriverError::modbus("illegal address");
        assert_eq!(format!("{}", e), "Modbus protocol error: illegal address");

        let e = DriverError::mapping("unknown tag");
        assert_eq!(format!("{}", e), "Mapping error: unknown tag");

        let e = DriverError::other("boom");
        assert_eq!(format!("{}", e), "Other: boom");
    }

    /// #feature DRV-MODBUS
    #[test]
    fn from_io_error() {
        let io = std::io::Error::other("network down");
        let de: DriverError = io.into();
        assert!(matches!(de, DriverError::Io(_)));
        assert!(format!("{}", de).contains("IO/network error"));
    }
}
