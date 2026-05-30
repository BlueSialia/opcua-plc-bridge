//! Write adapter types for the opcua-server crate.
//!
//! This module provides the lightweight trait and request types used by the OPC UA
//! adapter to forward client-initiated writes into the runtime. The goal is to
//! keep this API small and ergonomic so the runtime can implement the trait and
//! the OPC UA layer can remain agnostic of driver types.
//!
//! The trait returns a boxed future to avoid forcing implementors to depend on
//! `async_trait` while remaining fully async-friendly.

use futures::future::BoxFuture;
use futures::FutureExt;
use std::fmt;
use std::sync::Arc;

use crate::error::ServerError;
use core_model::TagValue;

/// Write request forwarded from the OPC UA layer into the runtime.
///
/// The `tag_id` must be a stable identifier understood by the runtime (for
/// example the same id strings exposed by `TagRegistry`/`TagDefinition`).
#[derive(Clone)]
pub struct WriteRequest {
    /// Stable runtime tag identifier.
    pub tag_id: String,

    /// Desired value to write.
    pub value: TagValue,
}

impl fmt::Debug for WriteRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteRequest")
            .field("tag_id", &self.tag_id)
            .finish_non_exhaustive()
    }
}

impl WriteRequest {
    /// Create a new write request.
    pub fn new(tag_id: impl Into<String>, value: TagValue) -> Self {
        Self {
            tag_id: tag_id.into(),
            value,
        }
    }
}

/// Trait implemented by the runtime to process writes originating from OPC UA.
///
/// Implementations should:
/// - Validate the request against the runtime/tag definitions (writability/type)
/// - Route the write to the correct driver or queue
/// - Optionally wait for driver confirmation (depending on configured ack mode)
///
/// Errors returned will be translated by the OPC UA adapter into appropriate
/// OPC UA status codes or diagnostics.
pub trait WriteHandler: Send + Sync + 'static {
    /// Handle an incoming write request.
    ///
    /// Implementations return a boxed future to remain ergonomic for both async
    /// and sync implementors.
    fn handle_write(&self, req: WriteRequest) -> BoxFuture<'static, Result<(), ServerError>>;
}

/// Blanket impl so closures and async functions are easy to use as handlers.
impl<F, Fut> WriteHandler for F
where
    F: Fn(WriteRequest) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), ServerError>> + Send + 'static,
{
    fn handle_write(&self, req: WriteRequest) -> BoxFuture<'static, Result<(), ServerError>> {
        (self)(req).boxed()
    }
}

/// Convenience shared pointer type for write handlers.
pub type WriteHandlerArc = Arc<dyn WriteHandler>;
