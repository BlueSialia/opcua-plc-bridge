//! Helpers to convert runtime `TagConfig` entries into core-model `TagDefinition`
//! and driver-specific mapping types (FINS / Modbus).
//!
//! The runtime configuration contains compact, user-friendly tag descriptions.
//! These functions validate and convert those descriptions into the strongly-typed
//! structures used by the rest of the system.
//!
//! Each function returns an `anyhow::Result` so callers can propagate
//! configuration problems with clear error messages.

use anyhow::{anyhow, Context, Result};

use crate::config::TagConfig;
use core_model::{TagDataType, TagDefinition, WordOrder};
use std::sync::Arc;

/// Parse a byte_order string into a `WordOrder`.
fn parse_byte_order(s: Option<&str>, context: &str) -> Result<WordOrder> {
    match s {
        None | Some("ABCD") | Some("abcd") => Ok(WordOrder::ABCD),
        Some("BADC") | Some("badc") => Ok(WordOrder::BADC),
        Some("CDAB") | Some("cdab") => Ok(WordOrder::CDAB),
        Some("DCBA") | Some("dcba") => Ok(WordOrder::DCBA),
        Some(other) => Err(anyhow!(
            "Unknown byte_order '{}' for {}; expected one of ABCD, BADC, CDAB, DCBA",
            other,
            context
        )),
    }
}

/// Parse textual `data_type` from config into `TagDataType`.
///
/// Centralizing this logic avoids duplication across the various mapping helpers.
fn parse_data_type(s: &str) -> Result<TagDataType> {
    match s.to_lowercase().as_str() {
        "bool" => Ok(TagDataType::Bool),
        "int16" => Ok(TagDataType::Int16),
        "uint16" => Ok(TagDataType::UInt16),
        "int32" => Ok(TagDataType::Int32),
        "uint32" => Ok(TagDataType::UInt32),
        "int64" => Ok(TagDataType::Int64),
        "uint64" => Ok(TagDataType::UInt64),
        "float" => Ok(TagDataType::Float),
        "double" => Ok(TagDataType::Double),
        "string" => Ok(TagDataType::String),
        "datetime" => Ok(TagDataType::DateTime),
        "bytestring" | "byte_string" => Ok(TagDataType::ByteString),
        other => Err(anyhow!("Unsupported data type: {}", other)),
    }
}

/// Convert a `TagConfig` (from the runtime config file) into a
/// `core_model::TagDefinition`, associating it with the given `plc_name`.
///
/// This performs validation of the textual `data_type` and maps an optional
/// `byte_order` hint into `ByteOrderConfig`.
pub fn tagconfig_to_definition(t: &TagConfig, plc_name: &str) -> Result<TagDefinition> {
    let dt = parse_data_type(&t.data_type)?;

    let byte_order = parse_byte_order(t.byte_order.as_deref(), "tag definition")?;

    let mut def = TagDefinition::new(
        t.id.clone(),
        t.name.clone(),
        t.address.clone(),
        dt,
        Arc::from(plc_name),
    );
    def.writable = t.writable;
    def.byte_order = byte_order;
    def.metadata = None;
    Ok(def)
}

/// Build a FINS driver `TagMapping` from a `TagConfig`.
///
/// When `t.area` is `Some`, use that explicit area code. Otherwise infer from address:
///   - `D100` or `d100` -> explicit D memory area (area 0x82)
///   - `100`            -> assume D memory (area 0x82)
pub fn tagconfig_to_fins_mapping(t: &TagConfig) -> Result<driver_fins::TagMapping> {
    let addr = t.address.trim();
    if addr.is_empty() {
        return Err(anyhow!("Empty address for FINS mapping"));
    }

    // Use explicit area if provided; otherwise infer from address string convention.
    let (area, address) = if let Some(explicit_area) = t.area {
        // When area is explicit, interpret the address as a numeric register offset.
        let raw = addr
            .parse::<u32>()
            .with_context(|| format!("Invalid FINS address '{}'", addr))?;
        (explicit_area, raw)
    } else {
        // Accept either 'D<number>' or pure digits (assume D-area)
        let (area, number_str) = if addr.starts_with('D') || addr.starts_with('d') {
            let rest = &addr[1..];
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                return Err(anyhow!(
                    "Invalid FINS D-address '{}'; expected 'D<digits>'",
                    addr
                ));
            }
            // When explicit area is set, use it; otherwise default to D (0x82)
            (0x82u8, rest)
        } else if addr.chars().all(|c| c.is_ascii_digit()) {
            (0x82u8, addr)
        } else {
            return Err(anyhow!(
                "Unsupported FINS address pattern '{}'; expected 'D<number>' or '<number>'",
                addr
            ));
        };

        let address = number_str
            .parse::<u32>()
            .with_context(|| format!("Invalid FINS address '{}'", addr))?;
        (area, address)
    };

    let word_count = t.word_count.unwrap_or(1);

    // Resolve the TagDataType from textual config so the FINS mapping carries the explicit data type.
    let dt = parse_data_type(&t.data_type)?;

    // Validate word_count compatibility against the declared data type
    match dt {
        TagDataType::Float | TagDataType::Int32 | TagDataType::UInt32 if word_count < 2 => {
            return Err(anyhow!(
                "Incompatible word_count {} for data_type '{}' (needs >=2)",
                word_count,
                t.data_type
            ));
        }
        TagDataType::Double if word_count < 4 => {
            return Err(anyhow!(
                "Incompatible word_count {} for data_type 'double' (needs >=4)",
                word_count
            ));
        }
        _ => {}
    }

    let byte_order = parse_byte_order(t.byte_order.as_deref(), "FINS mapping")?;

    Ok(driver_fins::TagMapping {
        tag_id: Arc::from(t.id.clone()),
        area,
        address,
        bit_offset: 0,
        word_count,
        writable: t.writable,
        byte_order,
        data_type: dt,
    })
}

/// Build a Modbus driver `ModbusMapping` from a `TagConfig`.
///
/// Expects numeric addresses (register offsets). Chooses a sensible default
/// function: coils for Bool, holding registers otherwise.
pub fn tagconfig_to_modbus_mapping(t: &TagConfig) -> Result<driver_modbus::ModbusMapping> {
    let addr = t.address.trim();

    // Accept either register-offset notation (e.g. "0") or human Modbus notation
    // (e.g. "40001" for holding register 0). Parse as u32 first to avoid overflow
    // and then normalize human notation (>=40001 -> subtract 40001).
    let raw = addr
        .parse::<u32>()
        .with_context(|| format!("Invalid Modbus address '{}'", addr))?;
    let normalized = if raw >= 40001 { raw - 40001 } else { raw };
    if normalized > (u16::MAX as u32) {
        return Err(anyhow!(
            "Modbus address '{}' normalized to {} exceeds u16 range",
            addr,
            normalized
        ));
    }
    let address = normalized as u16;

    let quantity = t.word_count.unwrap_or(1);

    // Resolve the TagDataType from textual config so drivers decode based on the mapping type
    let data_type = parse_data_type(&t.data_type)?;

    // Choose default Modbus function based on the declared data type (bool -> coils)
    let function = match data_type {
        TagDataType::Bool => driver_modbus::ModbusFunction::Coils,
        _ => driver_modbus::ModbusFunction::HoldingRegisters,
    };

    // Validate word/quantity compatibility against the declared data type
    match data_type {
        TagDataType::Float | TagDataType::Int32 | TagDataType::UInt32 if quantity < 2 => {
            return Err(anyhow!(
                "Incompatible word_count {} for data_type '{}' (needs >=2)",
                quantity,
                t.data_type
            ));
        }
        TagDataType::Double if quantity < 4 => {
            return Err(anyhow!(
                "Incompatible word_count {} for data_type 'double' (needs >=4)",
                quantity
            ));
        }
        _ => {}
    }

    let byte_order = parse_byte_order(t.byte_order.as_deref(), "Modbus mapping")?;

    Ok(driver_modbus::ModbusMapping {
        tag_id: Arc::from(t.id.clone()),
        data_type,
        address,
        quantity,
        function,
        writable: t.writable,
        byte_order,
        bit_offset: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::WordOrder;

    /// #feature UA-ACCESS
    #[test]
    fn convert_definition_ok() {
        let t = TagConfig {
            id: "T".into(),
            name: "Tag".into(),
            address: "100".into(),
            data_type: "UInt16".into(),
            writable: true,
            byte_order: None,
            word_count: None,
            area: None,
        };
        let def = tagconfig_to_definition(&t, "PLC1").expect("convert");
        assert_eq!(def.id.as_ref(), "T");
        assert!(def.writable);
        assert_eq!(def.plc_name.as_ref(), "PLC1");
    }

    /// #feature DRV-FINS
    #[test]
    fn fins_mapping_parsing() {
        let t = TagConfig {
            id: "t1".into(),
            name: "t1".into(),
            address: "D100".into(),
            data_type: "UInt16".into(),
            writable: true,
            byte_order: Some("CDAB".into()),
            word_count: Some(1),
            area: None,
        };
        let m = tagconfig_to_fins_mapping(&t).expect("fins mapping");
        assert_eq!(m.tag_id.as_ref(), "t1");
        assert_eq!(m.area, 0x82);
        assert_eq!(m.address, 100);
        assert_eq!(m.byte_order, WordOrder::CDAB);
    }

    /// #feature DRV-FINS
    #[test]
    fn fins_mapping_explicit_area() {
        let t = TagConfig {
            id: "t2".into(),
            name: "t2".into(),
            address: "50".into(),
            data_type: "UInt16".into(),
            writable: false,
            byte_order: None,
            word_count: Some(1),
            area: Some(0x83), // CIO area
        };
        let m = tagconfig_to_fins_mapping(&t).expect("fins mapping with explicit area");
        assert_eq!(m.area, 0x83);
        assert_eq!(m.address, 50);
    }

    /// #feature DRV-MODBUS
    #[test]
    fn modbus_mapping_parsing() {
        let t = TagConfig {
            id: "m1".into(),
            name: "m1".into(),
            address: "40001".into(),
            data_type: "Float".into(),
            writable: false,
            byte_order: Some("BADC".into()),
            word_count: Some(2),
            area: None,
        };
        let m = tagconfig_to_modbus_mapping(&t).expect("modbus mapping");
        assert_eq!(m.tag_id.as_ref(), "m1");
        // Human Modbus notation 40001 should map to register offset 0
        assert_eq!(m.address, 0u16);
        assert_eq!(m.quantity, 2);
        assert_eq!(m.byte_order, WordOrder::BADC);
    }
}
