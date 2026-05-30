//! FINS per-tag mapping types.

use core_model::{TagDataType, WordOrder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Default word count for single-word mappings.
fn default_word_count() -> u16 {
    1
}

/// Default bit offset (0).
fn default_bit_offset() -> u8 {
    0
}

/// FINS per-tag mapping: how a core-model tag maps to a FINS memory area/address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagMapping {
    /// core-model tag id
    #[serde(with = "core_model::arcstr_serde")]
    pub tag_id: Arc<str>,

    /// FINS memory area code (protocol specific), e.g. 0x82 for D area.
    pub area: u8,

    /// Word address within the memory area (word-addressable).
    pub address: u32,

    /// Bit offset inside a word (0-15). Use `0` for word-aligned values.
    #[serde(default = "default_bit_offset")]
    pub bit_offset: u8,

    /// Number of 16-bit words to read/write for this tag.
    #[serde(default = "default_word_count")]
    pub word_count: u16,

    /// Whether this mapping allows writes.
    #[serde(default)]
    pub writable: bool,

    /// Word order for multi-word values.
    #[serde(default)]
    pub byte_order: WordOrder,

    /// Data type for this mapping (configuration-time type used for decoding).
    pub data_type: TagDataType,
}

impl TagMapping {
    /// Simple constructor taking owned values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tag_id: impl Into<Arc<str>>,
        area: u8,
        address: u32,
        bit_offset: u8,
        word_count: u16,
        writable: bool,
        byte_order: WordOrder,
        data_type: TagDataType,
    ) -> Self {
        Self {
            tag_id: tag_id.into(),
            area,
            address,
            bit_offset,
            word_count,
            writable,
            byte_order,
            data_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::WordOrder;

    /// #feature DRV-FINS
    #[test]
    fn default_helpers_work() {
        assert_eq!(default_word_count(), 1);
        assert_eq!(default_bit_offset(), 0);
    }

    /// #feature DRV-FINS
    #[test]
    fn new_mapping_fields() {
        let m = TagMapping::new(
            "PLC::Tag",
            0x82,
            123,
            0,
            2,
            true,
            WordOrder::ABCD,
            TagDataType::UInt16,
        );
        assert_eq!(m.tag_id.as_ref(), "PLC::Tag");
        assert_eq!(m.area, 0x82);
        assert_eq!(m.address, 123);
        assert_eq!(m.word_count, 2);
        assert!(m.writable);
    }
}
