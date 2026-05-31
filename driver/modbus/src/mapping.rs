//! Modbus mapping and function types.

use core_model::{TagDataType, WordOrder};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Default quantity (1 register/coil) used when the config omits `quantity`.
fn default_quantity() -> u16 {
    1
}

/// Default bit offset (0) used when the config omits `bit_offset`.
fn default_bit_offset() -> u8 {
    0
}

/// Modbus function / data type selector used by the driver to choose the proper
/// Modbus operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum ModbusFunction {
    /// Read/write coils (bit-level discrete outputs)
    Coils,
    /// Read discrete inputs (read-only bits)
    DiscreteInputs,
    /// Holding registers (read/write 16-bit words typically)
    HoldingRegisters,
    /// Input registers (read-only 16-bit words)
    InputRegisters,
}

impl fmt::Display for ModbusFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModbusFunction::Coils => write!(f, "Coils"),
            ModbusFunction::DiscreteInputs => write!(f, "DiscreteInputs"),
            ModbusFunction::HoldingRegisters => write!(f, "HoldingRegisters"),
            ModbusFunction::InputRegisters => write!(f, "InputRegisters"),
        }
    }
}

/// Per-tag mapping from a core-model tag id to Modbus addressing details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModbusMapping {
    /// Stable core-model tag identifier.
    #[serde(with = "core_model::arcstr_serde")]
    pub tag_id: Arc<str>,

    /// Data type for this mapping.
    pub data_type: TagDataType,

    /// 0-based Modbus address / register offset.
    pub address: u16,

    /// Number of registers or coils to read (default = 1).
    #[serde(default = "default_quantity")]
    pub quantity: u16,

    /// Which Modbus function to use for this mapping.
    pub function: ModbusFunction,

    /// Bit offset inside a word for boolean/bit-addressable tags (default = 0).
    #[serde(default = "default_bit_offset")]
    pub bit_offset: u8,

    /// Whether this tag accepts writes from higher layers.
    #[serde(default)]
    pub writable: bool,

    /// Byte/word ordering for multi-register values.
    #[serde(default)]
    pub byte_order: WordOrder,
}

impl ModbusMapping {
    /// Convenience constructor.
    ///
    /// Accepts any `impl Into<Arc<str>>` so callers can pass `Arc::from(...)`
    /// or string literals without extra allocations.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tag_id: impl Into<Arc<str>>,
        address: u16,
        quantity: u16,
        function: ModbusFunction,
        data_type: TagDataType,
        bit_offset: u8,
        writable: bool,
        byte_order: WordOrder,
    ) -> Self {
        Self {
            tag_id: tag_id.into(),
            data_type,
            address,
            quantity,
            function,
            bit_offset,
            writable,
            byte_order,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::WordOrder;

    /// #feature DRV-MODBUS
    #[test]
    fn default_helpers_work() {
        assert_eq!(default_quantity(), 1);
        assert_eq!(default_bit_offset(), 0);
    }

    /// #feature DRV-MODBUS
    #[test]
    fn create_mapping_roundtrip() {
        let m = ModbusMapping::new(
            "PLC::T",
            10,
            2,
            ModbusFunction::HoldingRegisters,
            TagDataType::UInt16,
            0,
            true,
            WordOrder::ABCD,
        );
        assert_eq!(m.tag_id.as_ref(), "PLC::T");
        assert_eq!(m.address, 10);
        assert_eq!(m.quantity, 2);
        assert!(m.writable);
    }

    /// #feature DRV-MODBUS
    #[test]
    fn modbus_function_display() {
        assert_eq!(format!("{}", ModbusFunction::Coils), "Coils");
        assert_eq!(
            format!("{}", ModbusFunction::HoldingRegisters),
            "HoldingRegisters"
        );
    }

    /// #feature DRV-MODBUS
    #[test]
    fn serde_roundtrip() {
        let m = ModbusMapping::new(
            "tag-a",
            0,
            1,
            ModbusFunction::Coils,
            TagDataType::Bool,
            0,
            false,
            WordOrder::ABCD,
        );
        let s = serde_json::to_string(&m).expect("serialize");
        let m2: ModbusMapping = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(m.tag_id.as_ref(), m2.tag_id.as_ref());
        assert_eq!(m.address, m2.address);
        assert_eq!(m.function, m2.function);
    }
}
