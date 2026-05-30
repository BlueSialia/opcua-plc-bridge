//! opcua-server
//!
//! Thin OPC UA adapter that exposes the workspace `TagRegistry` as an OPC UA
//! address space and forwards client-initiated writes into the runtime write
//! path. This crate is intentionally an infrastructure adapter and contains no
//! business logic — it reads tags from `core_model` and relies on the runtime
//! to perform writes and confirmations.

#![deny(unsafe_code)]
#![allow(missing_docs)]

use open62541::ua;
use std::sync::Arc;

mod config;
mod error;
mod native;
mod types;
mod writes;

/// Login callback invoked by the OPC UA access control plugin.
///
/// The callback receives the provided username and password blob and should
/// return a UA `StatusCode` indicating whether authentication succeeded
/// (`StatusCode::GOOD`) or the reason for rejection (e.g. `BADUSERACCESSDENIED`).
pub type LoginCallback =
    Arc<dyn Fn(&ua::String, &ua::ByteString) -> ua::StatusCode + Send + Sync + 'static>;

pub use crate::config::{CertificateConfig, OpcUaConfig, SecurityMode, SecurityPolicy, WriteMode};
pub use crate::error::ServerError;
pub use crate::types::{Server, ServerHandle};
pub use crate::writes::{WriteHandler, WriteRequest};
