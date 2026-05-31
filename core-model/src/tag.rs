//! Runtime tag state: value, quality, and timestamps.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::sync::Arc;

use crate::tag_value::{TagQuality, TagValue};

/// Runtime state for a tag.
#[derive(Debug, Clone)]
pub struct Tag {
    /// Current value of the tag (atomically reference-counted for lock-free reads).
    pub value: Arc<TagValue>,
    /// Quality of the tag value (Good, Stale, CommLost, etc.).
    pub quality: TagQuality,
    /// Timestamp of when the value was sourced from the PLC.
    pub source_timestamp: DateTime<Utc>,
    /// Timestamp of when the value was last updated in the server.
    pub server_timestamp: DateTime<Utc>,
}

impl Tag {
    /// Create a new Tag.
    pub fn new(value: TagValue) -> Self {
        let now = Utc::now();
        Self {
            value: Arc::new(value),
            quality: TagQuality::Initializing,
            source_timestamp: now,
            server_timestamp: now,
        }
    }

    /// Apply a read value and update timestamps.
    pub fn apply_read(&mut self, value: TagValue, quality: TagQuality, source_ts: DateTime<Utc>) {
        self.value = Arc::new(value);
        self.quality = quality;
        self.source_timestamp = source_ts;
        self.server_timestamp = Utc::now();
    }

    /// Set quality.
    pub fn set_quality(&mut self, quality: TagQuality) {
        self.quality = quality;
        self.server_timestamp = Utc::now();
    }

    /// Update server timestamp.
    pub fn touch(&mut self) {
        self.server_timestamp = Utc::now();
    }

    /// Return true if the source timestamp is stale.
    pub fn is_stale(&self, timeout: ChronoDuration) -> bool {
        let now = Utc::now();
        match now.signed_duration_since(self.source_timestamp) {
            d if d < ChronoDuration::zero() => true,
            d => d > timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_value::TagValue;
    use chrono::Duration;

    /// #feature UA-QUALITY
    #[test]
    fn new_tag_defaults_initializing() {
        let t = Tag::new(TagValue::UInt16(0));
        assert_eq!(t.quality, TagQuality::Initializing);
    }

    /// #feature UA-TS
    #[test]
    fn is_stale_detects_staleness() {
        let mut t = Tag::new(TagValue::Float(1.0));
        t.source_timestamp = Utc::now() - Duration::seconds(10);
        assert!(t.is_stale(Duration::seconds(1)));
        assert!(!t.is_stale(Duration::seconds(60)));
    }

    /// #feature UA-TS, UA-QUALITY
    #[test]
    fn apply_read_updates_values_and_timestamps() {
        let mut t = Tag::new(TagValue::UInt16(1));
        let before = t.server_timestamp;
        let src_ts = Utc::now();
        t.apply_read(TagValue::UInt16(42), TagQuality::Good, src_ts);
        assert_eq!(*t.value, TagValue::UInt16(42));
        assert_eq!(t.quality, TagQuality::Good);
        assert!(t.server_timestamp >= before);
        assert_eq!(t.source_timestamp, src_ts);
    }
}
