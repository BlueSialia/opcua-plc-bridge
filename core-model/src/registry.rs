use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::error::CoreError;
use crate::tag::Tag;
use crate::tag_definition::TagDefinition;
use crate::tag_definition_store::TagDefinitionStore;
use crate::tag_store::TagStore;
use crate::tag_value::{TagDataType, TagQuality, TagValue};

/// Combines the read-only `TagDefinitionStore` with the concurrent `TagStore`.
#[derive(Clone, Debug)]
pub struct TagRegistry {
    definitions: TagDefinitionStore,
    tags: TagStore,
}

impl TagRegistry {
    /// Build a TagRegistry from definitions.
    ///
    /// Validates definitions and populates runtime tags with zero-equivalent values.
    pub fn from_definitions(defs: &[TagDefinition]) -> Result<Self, CoreError> {
        let def_store = TagDefinitionStore::from_definitions(defs)?;
        let tag_store = TagStore::new();

        for def in def_store.all_definitions_sorted() {
            let initial_value = match def.data_type {
                TagDataType::Bool => TagValue::Bool(false),
                TagDataType::Int16 => TagValue::Int16(0),
                TagDataType::UInt16 => TagValue::UInt16(0),
                TagDataType::Int32 => TagValue::Int32(0),
                TagDataType::UInt32 => TagValue::UInt32(0),
                TagDataType::Int64 => TagValue::Int64(0),
                TagDataType::UInt64 => TagValue::UInt64(0),
                TagDataType::Float => TagValue::Float(0.0),
                TagDataType::Double => TagValue::Double(0.0),
                TagDataType::DateTime => TagValue::DateTime(Utc::now()),
                TagDataType::ByteString => TagValue::ByteString(Vec::new()),
                TagDataType::String => TagValue::String(String::new()),
            };
            let tag = Tag::new(initial_value);
            tag_store.insert(def.id_str().to_string(), tag);
        }

        Ok(Self {
            definitions: def_store,
            tags: tag_store,
        })
    }

    /// Return the definition store (cloneable).
    pub fn definitions(&self) -> TagDefinitionStore {
        self.definitions.clone()
    }

    /// Return the tag store (cloneable).
    pub fn tags(&self) -> TagStore {
        self.tags.clone()
    }

    /// Get a runtime `Tag` snapshot by id.
    pub fn get_tag(&self, id: &str) -> Result<Arc<Tag>, CoreError> {
        self.tags.get(id)
    }

    /// Get a TagDefinition by id.
    pub fn get_definition(&self, id: &str) -> Result<&TagDefinition, CoreError> {
        self.definitions.get(id)
    }

    /// Update a runtime tag value, validating against the configured data type.
    pub fn update_tag_value(
        &self,
        id: &str,
        value: TagValue,
        quality: impl Into<TagQuality>,
        source_timestamp: DateTime<Utc>,
    ) -> Result<Tag, CoreError> {
        let def = self.definitions.get(id)?;
        // Convert into a concrete TagQuality before forwarding to the TagStore.
        let tq: TagQuality = quality.into();
        self.tags
            .update_value_with_expected(id, value, tq, source_timestamp, def.data_type.clone())
    }

    /// Set only the quality for a tag.
    pub fn set_tag_quality(&self, id: &str, quality: TagQuality) -> Result<(), CoreError> {
        self.tags.set_quality(id, quality)
    }

    /// List all tag ids in deterministic sorted order.
    ///
    /// Return cloned `Arc<str>` entries so callers can cheaply retain shared
    /// references to id strings without allocating owned `String`s.
    pub fn list_ids_sorted(&self) -> Vec<Arc<str>> {
        self.tags.list_ids_sorted()
    }

    /// Return all TagDefinitions sorted by id.
    pub fn all_definitions_sorted(&self) -> Vec<TagDefinition> {
        self.definitions.all_definitions_sorted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_value::TagDataType;
    use chrono::Utc;

    /// #feature UA-READ, UA-WRITE
    #[test]
    fn registry_from_defs_and_basic_ops() {
        let defs = vec![
            TagDefinition::new("ns=1;s=a", "A", "D100", TagDataType::UInt16, "PLC"),
            TagDefinition::new("ns=1;s=b", "B", "D101", TagDataType::Float, "PLC"),
        ];

        let reg = TagRegistry::from_definitions(&defs).expect("build registry");
        let ids = reg.list_ids_sorted();
        let expected = vec![Arc::from("ns=1;s=a"), Arc::from("ns=1;s=b")];
        assert_eq!(ids, expected);

        let def = reg.get_definition("ns=1;s=a").expect("def exists");
        assert_eq!(def.name.as_ref(), "A");

        let tag_handle = reg.get_tag("ns=1;s=a").expect("tag exists");
        {
            assert_eq!(*tag_handle.value, TagValue::UInt16(0));
        }

        let updated = reg
            .update_tag_value(
                "ns=1;s=a",
                TagValue::UInt16(123),
                TagQuality::Good,
                Utc::now(),
            )
            .expect("update ok");
        assert_eq!(*updated.value, TagValue::UInt16(123));
        assert_eq!(updated.quality, TagQuality::Good);
    }
}
