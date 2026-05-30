use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Runtime value of a tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TagValue {
    Bool(bool),
    Int16(i16),
    UInt16(u16),
    Int32(i32),
    UInt32(u32),
    Int64(i64),
    UInt64(u64),
    Float(f32),
    Double(f64),
    String(String),
    DateTime(DateTime<Utc>),
    ByteString(Vec<u8>),
}

impl fmt::Display for TagValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagValue::Bool(v) => write!(f, "{}", v),
            TagValue::Int16(v) => write!(f, "{}", v),
            TagValue::UInt16(v) => write!(f, "{}", v),
            TagValue::Int32(v) => write!(f, "{}", v),
            TagValue::UInt32(v) => write!(f, "{}", v),
            TagValue::Int64(v) => write!(f, "{}", v),
            TagValue::UInt64(v) => write!(f, "{}", v),
            TagValue::Float(v) => write!(f, "{}", v),
            TagValue::Double(v) => write!(f, "{}", v),
            TagValue::String(v) => write!(f, "{}", v),
            TagValue::DateTime(dt) => write!(f, "{}", dt.to_rfc3339()),
            TagValue::ByteString(b) => write!(f, "{:02x?}", b),
        }
    }
}

impl TagValue {
    /// Return the `TagDataType` corresponding to this runtime value.
    pub fn data_type(&self) -> TagDataType {
        match self {
            TagValue::Bool(_) => TagDataType::Bool,
            TagValue::Int16(_) => TagDataType::Int16,
            TagValue::UInt16(_) => TagDataType::UInt16,
            TagValue::Int32(_) => TagDataType::Int32,
            TagValue::UInt32(_) => TagDataType::UInt32,
            TagValue::Int64(_) => TagDataType::Int64,
            TagValue::UInt64(_) => TagDataType::UInt64,
            TagValue::Float(_) => TagDataType::Float,
            TagValue::Double(_) => TagDataType::Double,
            TagValue::String(_) => TagDataType::String,
            TagValue::DateTime(_) => TagDataType::DateTime,
            TagValue::ByteString(_) => TagDataType::ByteString,
        }
    }

    /// Check whether this runtime value matches the provided `TagDataType`.
    pub fn matches(&self, expected: &TagDataType) -> bool {
        &self.data_type() == expected
    }
}

/// Configuration-oriented type descriptor used in `TagDefinition`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TagDataType {
    Bool,
    Int16,
    UInt16,
    Int32,
    UInt32,
    Int64,
    UInt64,
    Float,
    Double,
    String,
    DateTime,
    ByteString,
}

/// Byte/word ordering used when combining multiple 16-bit registers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum WordOrder {
    #[default]
    ABCD,
    BADC,
    CDAB,
    DCBA,
}

impl WordOrder {
    /// Reorder bytes in-place according to the configured ordering.
    pub fn apply_to_bytes(&self, bytes: &mut [u8]) {
        match self {
            WordOrder::ABCD => {}
            WordOrder::BADC => swap_bytes_in_words(bytes),
            WordOrder::CDAB => swap_word_pairs_in_4byte_chunks(bytes),
            WordOrder::DCBA => reverse_bytes_in_4byte_chunks(bytes),
        }
    }
}

fn swap_bytes_in_words(bytes: &mut [u8]) {
    let len = bytes.len();
    let mut i = 0usize;
    while i + 1 < len {
        bytes.swap(i, i + 1);
        i += 2;
    }
}

fn swap_word_pairs_in_4byte_chunks(bytes: &mut [u8]) {
    let len = bytes.len();
    let mut i = 0usize;
    while i + 3 < len {
        bytes.swap(i, i + 2);
        bytes.swap(i + 1, i + 3);
        i += 4;
    }
    // leave trailing 2-byte word unchanged
}

fn reverse_bytes_in_4byte_chunks(bytes: &mut [u8]) {
    let len = bytes.len();
    let mut i = 0usize;
    while i + 3 < len {
        bytes.swap(i, i + 3);
        bytes.swap(i + 1, i + 2);
        i += 4;
    }
    // if a 2-byte remainder exists, swap its bytes
    if len % 4 >= 2 {
        let rem_start = len - (len % 4);
        if rem_start + 1 < len {
            bytes.swap(rem_start, rem_start + 1);
        }
    }
}

/// Tag quality enum used by drivers and runtime to express detailed,
/// industrial-grade states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TagQuality {
    /// Value is fresh and valid.
    Good,

    /// The tag is still initializing / waiting for its first PLC value.
    #[default]
    Initializing,

    /// The value is stale (source timestamp older than configured threshold).
    Stale,

    /// Communication to the PLC or network is lost.
    CommLost,

    /// Misconfiguration detected (addressing, mapping, etc).
    ConfigError,

    /// Type mismatch detected during a write or mapping conversion.
    TypeMismatch,

    /// A substitute or fallback value is being supplied.
    Substitute,

    /// Generic unexpected error; carry a human-friendly description.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// #feature UA-TYPES
    #[test]
    fn matches_detection() {
        assert!(TagValue::UInt16(1).matches(&TagDataType::UInt16));
        assert!(!TagValue::UInt16(1).matches(&TagDataType::Float));
        assert!(TagValue::Int64(-5).matches(&TagDataType::Int64));
        assert!(TagValue::UInt64(10).matches(&TagDataType::UInt64));
    }

    /// #feature UA-TYPES
    #[test]
    fn data_type_inference() {
        assert_eq!(TagValue::Bool(true).data_type(), TagDataType::Bool);
        assert_eq!(TagValue::Float(1.5).data_type(), TagDataType::Float);
        assert_eq!(
            TagValue::String("x".into()).data_type(),
            TagDataType::String
        );
        // Construct epoch DateTime (seconds=0, nanos=0) without deprecated APIs.
        let now = Utc.timestamp_opt(0, 0).single().unwrap();
        assert_eq!(TagValue::DateTime(now).data_type(), TagDataType::DateTime);
        assert_eq!(
            TagValue::ByteString(vec![1, 2, 3]).data_type(),
            TagDataType::ByteString
        );
    }

    /// #feature UA-TYPES
    #[test]
    fn byte_vec_helpers() {
        let bytes = vec![0xAA, 0xBB, 0xCC, 0xDD];

        let mut b = bytes.clone();
        WordOrder::ABCD.apply_to_bytes(&mut b);
        assert_eq!(b, vec![0xAA, 0xBB, 0xCC, 0xDD]);

        let mut b = bytes.clone();
        WordOrder::BADC.apply_to_bytes(&mut b);
        assert_eq!(b, vec![0xBB, 0xAA, 0xDD, 0xCC]);

        let mut b = bytes.clone();
        WordOrder::CDAB.apply_to_bytes(&mut b);
        assert_eq!(b, vec![0xCC, 0xDD, 0xAA, 0xBB]);

        let mut b = bytes.clone();
        WordOrder::DCBA.apply_to_bytes(&mut b);
        assert_eq!(b, vec![0xDD, 0xCC, 0xBB, 0xAA]);

        let mut bytes_5 = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        WordOrder::CDAB.apply_to_bytes(&mut bytes_5);
        assert_eq!(bytes_5, vec![0xCC, 0xDD, 0xAA, 0xBB, 0xEE]);

        let mut bytes_6 = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        WordOrder::DCBA.apply_to_bytes(&mut bytes_6);
        assert_eq!(bytes_6, vec![0xDD, 0xCC, 0xBB, 0xAA, 0xFF, 0xEE]);
    }

    /// #feature UA-QUALITY
    #[test]
    fn tag_quality_default_is_initializing() {
        assert_eq!(TagQuality::default(), TagQuality::Initializing);
    }
}
