//! Tag definition: static configuration-time metadata for a tag.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::CoreError;
use crate::tag_value::{TagDataType, WordOrder};

/// TagDefinition represents the static, configuration-time metadata for a tag.
///
/// This structure is protocol-agnostic: the `address` field is an opaque string that
/// protocol drivers interpret (for example "D100" for FINS or "300" for Modbus registers).
///
/// - `id` should be a stable, deterministic identifier (e.g. an OPC UA NodeId string like `ns=1;s=PLC1.Tag1`).
/// - `name` is the human-friendly display name, stored here and not duplicated in runtime `Tag`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagDefinition {
    /// Stable identifier for this tag (used as the key in TagRegistry/TagDefinitionStore).
    #[serde(with = "crate::arcstr_serde")]
    pub id: Arc<str>,

    /// Human-friendly display name for the tag.
    #[serde(with = "crate::arcstr_serde")]
    pub name: Arc<str>,

    /// Protocol-specific address string (interpreted by drivers).
    #[serde(with = "crate::arcstr_serde")]
    pub address: Arc<str>,

    /// Data type for this tag (configuration view).
    pub data_type: TagDataType,

    /// Whether OPC UA clients are allowed to write this tag.
    #[serde(default)]
    pub writable: bool,

    /// Byte/word order configuration used when combining multiple 16-bit registers.
    #[serde(default)]
    pub byte_order: WordOrder,

    /// Optional per-tag staleness threshold in milliseconds. If present, the runtime
    /// may mark the tag as stale when the last source timestamp is older than this value.
    #[serde(default)]
    pub stale_after_ms: Option<u64>,

    /// Optional arbitrary metadata (driver-specific hints, units, scaling, etc.).
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,

    /// PLC name that owns this tag. Used by the OPC UA server to organize
    /// tags into PLC folders.
    #[serde(with = "crate::arcstr_serde")]
    pub plc_name: Arc<str>,
}

impl TagDefinition {
    /// Validate basic invariants of a tag definition.
    ///
    /// Returns `Ok(())` when the definition looks reasonable, or a `CoreError::InvalidConfig`
    /// describing the problem.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.id.as_ref().trim().is_empty() {
            return Err(CoreError::InvalidConfig("TagDefinition.id is empty".into()));
        }
        if self.name.as_ref().trim().is_empty() {
            return Err(CoreError::InvalidConfig(format!(
                "TagDefinition.name is empty for id '{}'",
                self.id.as_ref()
            )));
        }
        if self.address.as_ref().trim().is_empty() {
            return Err(CoreError::InvalidConfig(format!(
                "TagDefinition.address is empty for id '{}'",
                self.id.as_ref()
            )));
        }
        Ok(())
    }

    /// Convenience constructor.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        address: impl Into<String>,
        data_type: TagDataType,
        plc_name: impl Into<Arc<str>>,
    ) -> Self {
        TagDefinition {
            id: Arc::from(id.into()),
            name: Arc::from(name.into()),
            address: Arc::from(address.into()),
            data_type,
            writable: false,
            byte_order: WordOrder::default(),
            stale_after_ms: None,
            metadata: None,
            plc_name: plc_name.into(),
        }
    }

    /// id as &str.
    pub fn id_str(&self) -> &str {
        self.id.as_ref()
    }

    /// name as &str.
    pub fn name_str(&self) -> &str {
        self.name.as_ref()
    }

    /// address as &str.
    pub fn address_str(&self) -> &str {
        self.address.as_ref()
    }

    /// Per-tag staleness threshold in milliseconds (if configured).
    pub fn stale_after_ms(&self) -> Option<u64> {
        self.stale_after_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_value::TagDataType;

    /// #feature UA-ACCESS
    #[test]
    fn validate_ok() {
        let def = TagDefinition::new("ns=1;s=test", "Test", "D100", TagDataType::UInt16, "PLC");
        assert!(def.validate().is_ok());
    }

    /// #feature UA-ACCESS
    #[test]
    fn validate_empty_id() {
        let def = TagDefinition::new("", "Test", "D100", TagDataType::UInt16, "PLC");
        match def.validate() {
            Err(CoreError::InvalidConfig(_)) => {}
            other => panic!("expected InvalidConfig, got {:?}", other),
        }
    }

    /// #feature UA-ACCESS
    #[test]
    fn validate_empty_name() {
        let def = TagDefinition::new("id", "", "D100", TagDataType::UInt16, "PLC");
        match def.validate() {
            Err(CoreError::InvalidConfig(_)) => {}
            other => panic!("expected InvalidConfig, got {:?}", other),
        }
    }

    /// #feature UA-ACCESS
    #[test]
    fn validate_empty_address() {
        let def = TagDefinition::new("id", "Name", "", TagDataType::UInt16, "PLC");
        match def.validate() {
            Err(CoreError::InvalidConfig(_)) => {}
            other => panic!("expected InvalidConfig, got {:?}", other),
        }
    }

    /// #feature UA-ACCESS
    #[test]
    fn constructor_and_helpers() {
        let d = TagDefinition::new("a", "A", "addr", TagDataType::UInt16, "PLC");
        assert_eq!(d.id_str(), "a");
        assert_eq!(d.name_str(), "A");
        assert_eq!(d.address_str(), "addr");
    }
}
