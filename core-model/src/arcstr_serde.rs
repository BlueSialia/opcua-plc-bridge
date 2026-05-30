//! Serde helpers for serializing/deserializing `Arc<str>`.
//!
//! `Arc<str>` is used in `TagDefinition` and driver mapping types to reduce memory
//! footprint when many definitions share identical strings. It is not directly
//! supported by serde's derives, so this module provides the helper functions
//! with `#[serde(with = "arcstr_serde")]`.

use serde::{Deserialize, Deserializer, Serializer};
use std::borrow::Cow;
use std::sync::Arc;

pub fn serialize<S>(v: &Arc<str>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(v.as_ref())
}

pub fn deserialize<'de, D>(d: D) -> Result<Arc<str>, D::Error>
where
    D: Deserializer<'de>,
{
    // Deserialize into a Cow<'de, str> so serde can optimize borrowed strings
    // when possible, then convert into an owned Arc<str>.
    let cow: Cow<'de, str> = Deserialize::deserialize(d)?;
    Ok(Arc::from(cow.into_owned()))
}

/// Serde helpers for `Option<Arc<str>>`.
pub mod option {
    use super::*;

    pub fn serialize<S>(v: &Option<Arc<str>>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match v {
            Some(arc) => super::serialize(arc, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Option<Arc<str>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<Cow<'de, str>> = Option::deserialize(d)?;
        Ok(opt.map(|cow| Arc::from(cow.into_owned())))
    }
}
