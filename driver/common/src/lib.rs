//! Common driver traits and utilities shared by protocol driver crates.
//!
//! The primary goal of this crate is to expose a small, stable trait that all
//! protocol drivers (FINS, Modbus, ...) implement so the runtime/scheduler can
//! treat drivers uniformly while allowing each driver to use its own error
//! types.

#![warn(missing_docs)]

use async_trait::async_trait;
use core_model::{TagValue, WordOrder};
use serde_json::Value;

/// Apply the configured `WordOrder` to a byte buffer in-place, then return the buffer.
pub fn apply_byte_order(mut bytes: Vec<u8>, order: &WordOrder) -> Vec<u8> {
    order.apply_to_bytes(&mut bytes);
    bytes
}

/// A convenient boxed error type drivers and callers may use when they don't
/// need a concrete error enum.
pub type DynDriverError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Trait implemented by protocol drivers so the runtime scheduler can poll them.
///
/// This trait models the protocol-specific responsibilities: polling the PLC,
/// performing writes, decoding protocol payloads and updating the runtime
/// tag registry. It intentionally keeps a minimal async API surface so drivers
/// remain simple to implement.
///
/// Returning `DynDriverError` (a boxed dynamic error) keeps the trait lightweight
/// and avoids forcing a particular error enum into this common crate.
#[async_trait]
pub trait ProtocolDriver: Send + Sync {
    /// Perform pre-flight validation of the driver configuration and mappings.
    ///
    /// This should be called during initialization to fail fast if any
    /// misconfiguration (e.g. overlapping addresses, invalid types) is detected.
    fn validate(&self) -> Result<(), DynDriverError> {
        Ok(())
    }

    /// Perform a single poll/read cycle.
    ///
    /// Implementations should perform a single logical iteration: connect if
    /// necessary, perform reads, apply updates to the `TagRegistry`, drain pending
    /// writes, emit health/events and then return.
    async fn read_cycle(&self) -> Result<(), DynDriverError>;

    /// Submit a write for a single tag to be executed by the driver.
    ///
    /// `tag_id` is the runtime stable tag identifier (namespaced by runtime).
    /// `value` is the runtime `TagValue` representing the desired value.
    async fn submit_write(&self, tag_id: &str, value: TagValue) -> Result<(), DynDriverError>;

    /// Optional health probe that drivers can implement to return JSON-serializable
    /// health information about the PLC/connection. The default implementation
    /// returns `Ok(None)`.
    async fn health(&self) -> Result<Option<Value>, DynDriverError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyDriver;
    #[async_trait]
    impl ProtocolDriver for DummyDriver {
        fn validate(&self) -> Result<(), DynDriverError> {
            Ok(())
        }

        async fn read_cycle(&self) -> Result<(), DynDriverError> {
            // No-op successful cycle
            Ok(())
        }

        async fn submit_write(
            &self,
            _tag_id: &str,
            _value: TagValue,
        ) -> Result<(), DynDriverError> {
            // No-op write
            Ok(())
        }

        // use default health()
    }

    /// #feature DRV-MODBUS, UA-TYPES
    #[tokio::test]
    async fn dummy_driver_runs() {
        let d = DummyDriver;
        let res = d.validate();
        assert!(res.is_ok());

        let res = d.read_cycle().await;
        assert!(res.is_ok());

        let wres = d.submit_write("t", TagValue::UInt16(1)).await;
        assert!(wres.is_ok());

        let h = d.health().await.expect("health call ok");
        assert!(h.is_none());
    }
}
