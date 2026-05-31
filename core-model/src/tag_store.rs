//! Thread-safe runtime tag store using per-tag `ArcSwap` for lock-free reads.
//!
//! Structural metadata is protected by a single `RwLock`. Per-tag reads are
//! lock-free: callers clone an `Arc<ArcSwap<Tag>>` and call `load_full()` to get
//! a snapshot.

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, RwLock};

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use tracing::{debug, instrument, warn};

use crate::error::CoreError;
use crate::tag::Tag;
use crate::tag_value::{TagDataType, TagQuality, TagValue};

/// Internal contiguous storage guarded by a single `RwLock`.
///
/// `tags` stores per-tag atomic holders (`Arc<ArcSwap<Tag>>`),
/// `index` maps tag ids to indices (keys are `Arc<str>` to avoid repeated allocations),
/// and `sorted_ids` caches a sorted id list stored as `Arc<str>`.
///
/// Additionally `subscribers` stores a list of channels used to broadcast
/// `TagChange` events to any subscriber interested in runtime tag updates.
#[derive(Debug)]
struct Inner {
    tags: Vec<Arc<ArcSwap<Tag>>>,
    index: HashMap<Arc<str>, usize>,
    sorted_ids: Vec<Arc<str>>,
    subscribers: Vec<SyncSender<TagChange>>,
}

impl Inner {
    fn new() -> Self {
        Self {
            tags: Vec::new(),
            index: HashMap::new(),
            sorted_ids: Vec::new(),
            subscribers: Vec::new(),
        }
    }
}

/// Small event describing a tag change emitted by the runtime TagStore.
///
/// `tag` is an owned snapshot of the runtime `Tag` after the change.
#[derive(Debug, Clone)]
pub struct TagChange {
    /// Stable runtime tag identifier (owned).
    pub tag_id: String,

    /// Snapshot of the runtime `Tag` after the change.
    pub tag: Tag,
}

/// Runtime store for `Tag`s using per-tag `ArcSwap<Tag>` holders.
///
/// Structural metadata is protected by a single `RwLock<Inner>`. Per-tag reads are
/// lock-free: callers clone an `Arc<ArcSwap<Tag>>` and call `load_full()` to get a snapshot.
#[derive(Clone, Debug)]
pub struct TagStore {
    inner: Arc<RwLock<Inner>>,
}

impl TagStore {
    /// Create a new, empty TagStore.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::new())),
        }
    }

    /// Publish a `TagChange` to all subscribers (best-effort, non-blocking).
    fn broadcast_change(&self, tag_id: &str, tag: &Tag) {
        let inner = self.inner.read().unwrap();
        Self::broadcast_change_locked(&inner, tag_id, tag);
    }

    /// Same as `broadcast_change` but operates on an already-locked `Inner` reference.
    fn broadcast_change_locked(inner: &Inner, tag_id: &str, tag: &Tag) {
        for tx in inner.subscribers.iter() {
            match tx.try_send(TagChange {
                tag_id: tag_id.to_string(),
                tag: tag.clone(),
            }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    warn!(tag = %tag_id, "Tag change event dropped: subscriber channel full");
                }
                Err(TrySendError::Disconnected(_)) => {
                    // Subscriber disconnected; will be pruned on next write-lock
                }
            }
        }
    }

    /// Insert or replace a runtime `Tag` under `id`.
    ///
    /// If `id` exists the per-tag `ArcSwap` is reused and its value replaced atomically.
    #[instrument(skip(self, id, tag))]
    pub fn insert(&self, id: impl Into<String>, tag: Tag) {
        let id_owned = id.into();
        let arc_id: Arc<str> = Arc::from(id_owned);

        debug!(id = %arc_id.as_ref(), "Inserting runtime tag into TagStore");

        // Fast path: if id exists, avoid write lock by cloning per-slot handle.
        {
            let inner = self.inner.read().unwrap();
            if let Some(&idx) = inner.index.get(arc_id.as_ref()) {
                let handle = inner.tags[idx].clone();
                drop(inner);
                // Store the tag and publish change to subscribers
                handle.as_ref().store(Arc::new(tag.clone()));
                self.broadcast_change(arc_id.as_ref(), &tag);
                return;
            }
        }

        // Slow path: acquire write lock to modify structure.
        let mut inner = self.inner.write().unwrap();
        if let Some(&idx) = inner.index.get(arc_id.as_ref()) {
            // Race: inserted by another thread while upgrading locks.
            inner.tags[idx].as_ref().store(Arc::new(tag));
        } else {
            let idx = inner.tags.len();
            inner
                .tags
                .push(Arc::new(ArcSwap::from_pointee(tag.clone())));
            // Store a clone of the Arc<str> as key in the index
            inner.index.insert(arc_id.clone(), idx);

            // Insert into `sorted_ids` at the correct position to keep it sorted
            // using binary search instead of sorting the whole vector every time.
            // Compare by string contents.
            let insert_pos = match inner
                .sorted_ids
                .binary_search_by(|a| a.as_ref().cmp(arc_id.as_ref()))
            {
                Ok(pos) => pos, // equal keys shouldn't happen because `index` was checked above
                Err(pos) => pos,
            };
            // Preserve an owned string copy of the id before moving `arc_id` into `sorted_ids`.
            let tag_id_owned = arc_id.as_ref().to_string();
            inner.sorted_ids.insert(insert_pos, arc_id);

            let tag = inner.tags[idx].as_ref().load_full();
            Self::broadcast_change_locked(&inner, tag_id_owned.as_str(), &tag);
        }
    }

    /// Return a snapshot (`Arc<Tag>`) for `id`.
    ///
    /// `id` can be any `&str` (borrowed lookup is performed against `Arc<str>` keys).
    pub fn get(&self, id: &str) -> Result<Arc<Tag>, CoreError> {
        let inner = self.inner.read().unwrap();
        if let Some(&idx) = inner.index.get(id) {
            Ok(inner.tags[idx].as_ref().load_full())
        } else {
            Err(CoreError::TagNotFound(id.to_string()))
        }
    }

    /// Subscribe to tag change events.
    ///
    /// Returns a `std::sync::mpsc::Receiver<TagChange>` which will receive a
    /// `TagChange` each time a tag is inserted/updated/modified. Subscribers
    /// are stored in the TagStore and will be notified on subsequent updates.
    pub fn subscribe(&self) -> Receiver<TagChange> {
        let (tx, rx) = sync_channel(128);
        // Register sender with the store so it will receive future events.
        let mut inner = self.inner.write().unwrap();
        inner.subscribers.push(tx);
        rx
    }

    /// Update a tag's value with authoritative type checking.
    ///
    /// `expected` is the configured `TagDataType` (from `TagDefinition`).
    #[instrument(skip(self, value, source_ts), fields(id = %id))]
    pub fn update_value_with_expected(
        &self,
        id: &str,
        value: TagValue,
        quality: TagQuality,
        source_ts: DateTime<Utc>,
        expected: TagDataType,
    ) -> Result<Tag, CoreError> {
        if !value.matches(&expected) {
            return Err(CoreError::TypeMismatch(id.to_string()));
        }

        // Find per-slot handle under a read lock and clone it.
        let handle = {
            let inner = self.inner.read().unwrap();
            if let Some(&idx) = inner.index.get(id) {
                inner.tags[idx].clone()
            } else {
                return Err(CoreError::TagNotFound(id.to_string()));
            }
        };

        // Create mutated Tag and publish atomically.
        let current_arc = handle.as_ref().load_full();
        let mut new_tag = (*current_arc).clone();
        new_tag.apply_read(value, quality, source_ts);
        // Store a fresh Arc<Tag> into the ArcSwap
        handle.as_ref().store(Arc::new(new_tag.clone()));

        self.broadcast_change(id, &new_tag);

        Ok(new_tag)
    }

    /// Set only the quality of an existing tag.
    pub fn set_quality(&self, id: &str, quality: impl Into<TagQuality>) -> Result<(), CoreError> {
        let handle = {
            let inner = self.inner.read().unwrap();
            if let Some(&idx) = inner.index.get(id) {
                inner.tags[idx].clone()
            } else {
                return Err(CoreError::TagNotFound(id.to_string()));
            }
        };

        let tag_quality: TagQuality = quality.into();
        let current_arc = handle.as_ref().load_full();
        let mut new_tag = (*current_arc).clone();
        new_tag.set_quality(tag_quality);
        handle.as_ref().store(Arc::new(new_tag.clone()));

        self.broadcast_change(id, &new_tag);

        Ok(())
    }

    /// List all tag ids in deterministic sorted order (cached).
    ///
    /// Returns cloned `Arc<str>` entries so callers can cheaply retain shared
    /// references to id strings without extra allocations.
    pub fn list_ids_sorted(&self) -> Vec<Arc<str>> {
        let inner = self.inner.read().unwrap();
        inner.sorted_ids.clone()
    }

    /// Apply a closure to a cloned snapshot of the Tag and store the mutated value.
    pub fn try_update_with<F, R>(&self, id: &str, mut f: F) -> Result<R, CoreError>
    where
        F: FnMut(&mut Tag) -> Result<R, CoreError>,
    {
        let handle = {
            let inner = self.inner.read().unwrap();
            if let Some(&idx) = inner.index.get(id) {
                inner.tags[idx].clone()
            } else {
                return Err(CoreError::TagNotFound(id.to_string()));
            }
        };

        let current_arc = handle.as_ref().load_full();
        let mut t = (*current_arc).clone();
        let res = f(&mut t)?;
        t.touch();
        handle.as_ref().store(Arc::new(t.clone()));

        self.broadcast_change(id, &t);

        Ok(res)
    }

    /// Return the number of tags in the store.
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.tags.len()
    }

    /// Return true if the store contains no tags.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for TagStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_value::TagValue;

    /// #feature UA-READ
    #[test]
    fn insert_and_get() {
        let store = TagStore::new();
        store.insert("a", Tag::new(TagValue::UInt16(1)));
        let t = store.get("a").expect("exists");
        assert_eq!(*t.value, TagValue::UInt16(1));
    }

    /// #feature UA-READ
    #[test]
    fn update_and_snapshot() {
        let store = TagStore::new();
        store.insert("x", Tag::new(TagValue::Float(0.0)));
        let updated = store
            .update_value_with_expected(
                "x",
                TagValue::Float(std::f32::consts::PI),
                TagQuality::Good,
                Utc::now(),
                TagDataType::Float,
            )
            .expect("update ok");
        assert_eq!(*updated.value, TagValue::Float(std::f32::consts::PI));
        let s = (*store.get("x").expect("snapshot")).clone();
        assert_eq!(*s.value, TagValue::Float(std::f32::consts::PI));
    }

    /// #feature UA-WRITE
    #[test]
    fn try_update_with_applies_changes() {
        let store = TagStore::new();
        store.insert("u", Tag::new(TagValue::UInt16(2)));
        store
            .try_update_with("u", |t| {
                // With `value` stored as `Arc<TagValue>` the closure replaces the value atomically.
                if let TagValue::UInt16(_) = &*t.value {
                    t.value = Arc::new(TagValue::UInt16(10));
                    Ok(())
                } else {
                    Err(CoreError::TypeMismatch("u".into()))
                }
            })
            .expect("applied");
        let snapshot = (*store.get("u").expect("snapshot")).clone();
        assert_eq!(*snapshot.value, TagValue::UInt16(10));
    }
}
