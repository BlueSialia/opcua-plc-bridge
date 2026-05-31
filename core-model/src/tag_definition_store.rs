//! Read-only store for validated tag definitions.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::CoreError;
use crate::tag_definition::TagDefinition;

/// Read-only store for `TagDefinition`s.
#[derive(Clone, Debug)]
pub struct TagDefinitionStore {
    inner: Arc<HashMap<Arc<str>, TagDefinition>>,
}

impl TagDefinitionStore {
    /// Build store from definitions.
    pub fn from_definitions(defs: &[TagDefinition]) -> Result<Self, CoreError> {
        let mut map: HashMap<Arc<str>, TagDefinition> = HashMap::with_capacity(defs.len());
        for d in defs {
            d.validate()?;
            // Use the `&str` view for duplicate checks (Arc<str> borrows as &str).
            if map.contains_key(d.id.as_ref()) {
                return Err(CoreError::InvalidConfig(format!(
                    "Duplicate TagDefinition id: {}",
                    d.id.as_ref()
                )));
            }
            map.insert(d.id.clone(), d.clone());
        }
        Ok(Self {
            inner: Arc::new(map),
        })
    }

    /// Number of definitions.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Get definition by id (borrowed lookup by &str).
    pub fn get(&self, id: &str) -> Result<&TagDefinition, CoreError> {
        self.inner
            .get(id)
            .ok_or_else(|| CoreError::DefinitionNotFound(id.to_string()))
    }

    /// Whether a definition with `id` exists.
    pub fn contains(&self, id: &str) -> bool {
        self.inner.contains_key(id)
    }

    /// All definitions (sorted).
    pub fn all_definitions_sorted(&self) -> Vec<TagDefinition> {
        let mut defs: Vec<TagDefinition> = self.inner.values().cloned().collect();
        defs.sort_by(|a, b| a.id.as_ref().cmp(b.id.as_ref()));
        defs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_value::TagDataType;

    /// #feature UA-ACCESS
    #[test]
    fn create_and_query_store() {
        let defs = vec![
            TagDefinition::new("a", "A", "D100", TagDataType::UInt16, "PLC"),
            TagDefinition::new("b", "B", "D102", TagDataType::Float, "PLC"),
        ];

        let store = TagDefinitionStore::from_definitions(&defs).expect("build store");
        assert_eq!(store.len(), 2);
        assert!(store.contains("a"));
        let a = store.get("a").expect("exists");
        assert_eq!(a.name.as_ref(), "A");

        let ids: Vec<String> = store
            .all_definitions_sorted()
            .iter()
            .map(|d| d.id.as_ref().to_string())
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    /// #feature UA-ACCESS
    #[test]
    fn duplicate_definition_fails() {
        let defs = vec![
            TagDefinition::new("a", "A", "D100", TagDataType::UInt16, "PLC"),
            TagDefinition::new("a", "A2", "D200", TagDataType::UInt16, "PLC"),
        ];
        let res = TagDefinitionStore::from_definitions(&defs);
        assert!(res.is_err());
    }
}
