//! Core model: protocol-agnostic tag definitions, runtime tags, stores and errors.

#![warn(missing_docs)]

pub mod arcstr_serde;
pub mod error;
pub mod registry;
pub mod tag;
pub mod tag_definition;
pub mod tag_definition_store;
pub mod tag_store;
pub mod tag_value;

pub use error::*;
pub use registry::*;
pub use tag::*;
pub use tag_definition::*;
pub use tag_definition_store::*;
pub use tag_store::*;
pub use tag_value::*;
