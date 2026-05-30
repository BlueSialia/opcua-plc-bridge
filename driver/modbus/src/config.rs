//! Modbus driver configuration.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use crate::mapping::ModbusMapping;

/// Default keepalive seconds when omitted.
fn default_keepalive_secs() -> u64 {
    30
}

/// Default maximum reconnect backoff seconds when omitted.
fn default_max_backoff_secs() -> u64 {
    30
}

/// Default IO timeout in milliseconds for Modbus operations.
fn default_io_timeout_ms() -> u64 {
    2000
}

/// Configuration for a Modbus/TCP driver instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModbusConfig {
    /// Logical name for the PLC / driver instance.
    pub name: String,

    /// TCP endpoint of the PLC (e.g. `127.0.0.1:502`).
    pub endpoint: SocketAddr,

    /// Modbus unit/slave id (1-255). Required for multi-slave environments.
    pub unit_id: u8,

    /// Poll cycle in milliseconds.
    pub cycle_ms: u64,

    /// Per-tag mappings for this PLC instance.
    pub mappings: Vec<ModbusMapping>,

    /// TCP keepalive in seconds (default: 30).
    #[serde(default = "default_keepalive_secs")]
    pub keepalive_secs: u64,

    /// Maximum reconnect backoff in seconds (default: 30).
    #[serde(default = "default_max_backoff_secs")]
    pub max_backoff_secs: u64,

    /// IO timeout in milliseconds for Modbus operations (default: 2000).
    #[serde(default = "default_io_timeout_ms")]
    pub io_timeout_ms: u64,
}

impl ModbusConfig {
    /// Convenience constructor used in tests and programmatic creation.
    pub fn new(
        name: impl Into<String>,
        endpoint: SocketAddr,
        unit_id: u8,
        cycle_ms: u64,
        mappings: Vec<ModbusMapping>,
    ) -> Self {
        Self {
            name: name.into(),
            endpoint,
            unit_id,
            cycle_ms,
            mappings,
            keepalive_secs: default_keepalive_secs(),
            max_backoff_secs: default_max_backoff_secs(),
            io_timeout_ms: default_io_timeout_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::ModbusFunction;
    use core_model::WordOrder;
    use std::str::FromStr;

    /// #feature DRV-MODBUS
    #[test]
    fn defaults_are_set() {
        let cfg = ModbusConfig {
            name: "plc-x".into(),
            endpoint: SocketAddr::from_str("127.0.0.1:502").unwrap(),
            unit_id: 1,
            cycle_ms: 100,
            mappings: vec![],
            keepalive_secs: default_keepalive_secs(),
            max_backoff_secs: default_max_backoff_secs(),
            io_timeout_ms: default_io_timeout_ms(),
        };

        assert_eq!(cfg.keepalive_secs, 30);
        assert_eq!(cfg.max_backoff_secs, 30);
        assert_eq!(cfg.io_timeout_ms, 2000);
    }

    /// #feature DRV-MODBUS
    #[test]
    fn create_with_new_and_mapping() {
        let mapping = crate::mapping::ModbusMapping::new(
            "PLC::Tag",
            0,
            1,
            ModbusFunction::HoldingRegisters,
            core_model::TagDataType::UInt16,
            0,
            true,
            WordOrder::ABCD,
        );

        let cfg = ModbusConfig::new(
            "plc-1",
            SocketAddr::from_str("10.0.0.1:502").unwrap(),
            1,
            200,
            vec![mapping],
        );

        assert_eq!(cfg.name, "plc-1");
        assert_eq!(cfg.cycle_ms, 200);
        assert_eq!(cfg.mappings.len(), 1);
    }
}
