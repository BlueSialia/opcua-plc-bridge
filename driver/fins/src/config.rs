//! FINS driver configuration.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use crate::mapping::TagMapping;

/// Default keepalive seconds when omitted.
fn default_keepalive_secs() -> u64 {
    30
}

/// Default maximum reconnect backoff seconds when omitted.
fn default_max_backoff_secs() -> u64 {
    30
}

/// Default maximum FINS words per request.
fn default_max_fins_words() -> u32 {
    960
}

/// Configuration for a FINS/TCP driver instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinsConfig {
    /// Logical name for the PLC / driver instance.
    pub name: String,

    /// TCP endpoint of the PLC (e.g. "192.168.1.10:9600").
    pub endpoint: SocketAddr,

    /// Poll cycle in milliseconds.
    pub cycle_ms: u64,

    /// TCP keepalive in seconds (default: 30).
    #[serde(default = "default_keepalive_secs")]
    pub keepalive_secs: u64,

    /// Maximum reconnect backoff in seconds (default: 30).
    #[serde(default = "default_max_backoff_secs")]
    pub max_backoff_secs: u64,

    /// Per-tag mappings for this PLC instance.
    pub mappings: Vec<TagMapping>,

    /// Max words per FINS read/write request (default provided).
    #[serde(default = "default_max_fins_words")]
    pub max_words_per_request: u32,
}

impl FinsConfig {
    /// Convenience constructor.
    pub fn new(
        name: impl Into<String>,
        endpoint: SocketAddr,
        cycle_ms: u64,
        mappings: Vec<TagMapping>,
    ) -> Self {
        Self {
            name: name.into(),
            endpoint,
            cycle_ms,
            keepalive_secs: default_keepalive_secs(),
            max_backoff_secs: default_max_backoff_secs(),
            mappings,
            max_words_per_request: default_max_fins_words(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::TagMapping;
    use core_model::{TagDataType, WordOrder};
    use std::str::FromStr;

    /// #feature DRV-FINS
    #[test]
    fn default_values_are_set() {
        let cfg = FinsConfig {
            name: "plc-x".into(),
            endpoint: SocketAddr::from_str("127.0.0.1:9600").unwrap(),
            cycle_ms: 100,
            keepalive_secs: default_keepalive_secs(),
            max_backoff_secs: default_max_backoff_secs(),
            mappings: vec![],
            max_words_per_request: default_max_fins_words(),
        };
        assert_eq!(cfg.keepalive_secs, 30);
        assert_eq!(cfg.max_backoff_secs, 30);
    }

    /// #feature DRV-FINS
    #[test]
    fn create_with_new_and_mapping() {
        let mapping = TagMapping::new(
            "PLC::Tag",
            0x82,
            100,
            0,
            1,
            true,
            WordOrder::ABCD,
            TagDataType::UInt16,
        );
        let cfg = FinsConfig::new(
            "plc-1",
            SocketAddr::from_str("10.0.0.1:9600").unwrap(),
            200,
            vec![mapping],
        );
        assert_eq!(cfg.name, "plc-1");
        assert_eq!(cfg.cycle_ms, 200);
        assert_eq!(cfg.mappings.len(), 1);
    }
}
