//! Runtime write handler: validates writes, routes to drivers, and manages confirmations.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use tokio::sync::{mpsc, oneshot};
use tokio::time;

use core_model::{TagRegistry, TagValue};

/// Driver senders used by the write handler. Each variant holds the typed sender
/// that corresponds to the driver's `WriteRequest` type.
#[derive(Clone)]
enum DriverSender {
    Fins(mpsc::Sender<driver_fins::WriteRequest>),
    Modbus(mpsc::Sender<driver_modbus::WriteRequest>),
}

/// Runtime write handler: validate, route, and confirm writes.
pub struct RuntimeWriteHandler {
    /// Map of tag_id -> driver sender.
    routing: Arc<HashMap<Arc<str>, DriverSender>>,

    /// TagRegistry gives access to definitions and runtime tags for validation and lookups.
    registry: Arc<TagRegistry>,

    /// Runtime timeout to wait for driver confirmation.
    confirm_timeout: Duration,

    /// Timeout to use when sending into a driver's mpsc write queue.
    driver_send_timeout: Duration,
}

impl RuntimeWriteHandler {
    /// Create a new handler.
    pub fn new(registry: Arc<TagRegistry>, confirm_timeout: Duration) -> Self {
        Self {
            routing: Arc::new(HashMap::new()),
            registry,
            confirm_timeout,
            driver_send_timeout: Duration::from_secs(2),
        }
    }

    /// Set driver send timeout.
    pub fn with_driver_send_timeout(mut self, timeout: Duration) -> Self {
        self.driver_send_timeout = timeout;
        self
    }

    /// Add FINS route. Uses `Arc::make_mut` to clone-on-write so we can mutate
    /// the shared routing map while keeping read access cheap for all callers.
    pub fn add_route_for_fins(
        &mut self,
        tag_id: Arc<str>,
        sender: mpsc::Sender<driver_fins::WriteRequest>,
    ) {
        Arc::make_mut(&mut self.routing).insert(tag_id, DriverSender::Fins(sender));
    }

    /// Add Modbus route.
    pub fn add_route_for_modbus(
        &mut self,
        tag_id: Arc<str>,
        sender: mpsc::Sender<driver_modbus::WriteRequest>,
    ) {
        Arc::make_mut(&mut self.routing).insert(tag_id, DriverSender::Modbus(sender));
    }
}

impl opcua_server::WriteHandler for RuntimeWriteHandler {
    /// Validate, route, enqueue, and optionally await confirmation for a write request.
    ///
    /// Flow: check type compatibility → check writability → look up driver route →
    /// send the write request via the driver's mpsc channel → wait for confirmation
    /// (with timeout) and return the result.
    fn handle_write(
        &self,
        req: opcua_server::WriteRequest,
    ) -> BoxFuture<'static, Result<(), opcua_server::ServerError>> {
        let tag_id = req.tag_id;
        let value = req.value;
        let registry = Arc::clone(&self.registry);
        let routing = Arc::clone(&self.routing);
        let confirm_timeout = self.confirm_timeout;
        let driver_send_timeout = self.driver_send_timeout;

        async move {
            // Validate the request against registry/definitions
            {
                // Check runtime tag exists and variant-kind matches
                match registry.tags().get(&tag_id) {
                    Ok(tag) => {
                        use TagValue::*;
                        let ok = matches!(
                            (&*tag.value, &value),
                            (Bool(_), Bool(_))
                                | (Int16(_), Int16(_))
                                | (UInt16(_), UInt16(_))
                                | (Int32(_), Int32(_))
                                | (UInt32(_), UInt32(_))
                                | (Int64(_), Int64(_))
                                | (UInt64(_), UInt64(_))
                                | (Float(_), Float(_))
                                | (Double(_), Double(_))
                                | (String(_), String(_))
                                | (DateTime(_), DateTime(_))
                                | (ByteString(_), ByteString(_))
                        );
                        if !ok {
                            return Err(opcua_server::ServerError::Other(format!(
                                "Type mismatch for tag {}",
                                tag_id
                            )));
                        }
                    }
                    Err(e) => {
                        return Err(opcua_server::ServerError::Other(format!(
                            "Runtime tag not found {}: {}",
                            tag_id, e
                        )));
                    }
                }

                // Validate definition (writability)
                match registry.get_definition(&tag_id) {
                    Ok(def) => {
                        if !def.writable {
                            return Err(opcua_server::ServerError::Other(format!(
                                "Tag {} is not writable",
                                tag_id
                            )));
                        }
                    }
                    Err(_) => {
                        return Err(opcua_server::ServerError::Other(format!(
                            "Tag definition not found for {}",
                            tag_id
                        )));
                    }
                }
            }

            // Find route (routing keys are `Arc<str>`; lookup by &str without allocating Arc)
            let sender = match routing.get(tag_id.as_str()) {
                Some(s) => s.clone(),
                None => {
                    return Err(opcua_server::ServerError::Other(format!(
                        "No driver route for tag {}",
                        tag_id
                    )));
                }
            };

            // Create oneshot for driver-level confirmation and send request with reply channel.
            let (reply_tx, reply_rx) = oneshot::channel::<Result<(), String>>();

            match sender {
                DriverSender::Fins(tx) => {
                    let drv_req = driver_fins::WriteRequest {
                        tag_id: tag_id.clone(),
                        value: value.clone(),
                        reply: Some(reply_tx),
                    };
                    match time::timeout(driver_send_timeout, tx.send(drv_req)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            return Err(opcua_server::ServerError::Other(format!(
                                "Failed to enqueue FINS write: {}",
                                e
                            )));
                        }
                        Err(_) => {
                            return Err(opcua_server::ServerError::Other(
                                "Timed out enqueuing FINS write".into(),
                            ));
                        }
                    }
                }
                DriverSender::Modbus(tx) => {
                    let drv_req = driver_modbus::WriteRequest {
                        tag_id: tag_id.clone(),
                        value: value.clone(),
                        reply: Some(reply_tx),
                    };
                    match time::timeout(driver_send_timeout, tx.send(drv_req)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            return Err(opcua_server::ServerError::Other(format!(
                                "Failed to enqueue Modbus write: {}",
                                e
                            )));
                        }
                        Err(_) => {
                            return Err(opcua_server::ServerError::Other(
                                "Timed out enqueuing Modbus write".into(),
                            ));
                        }
                    }
                }
            }

            // Wait for driver-level confirmation with a runtime timeout
            match time::timeout(confirm_timeout, reply_rx).await {
                Ok(Ok(Ok(()))) => Ok(()),
                Ok(Ok(Err(err_msg))) => Err(opcua_server::ServerError::Other(format!(
                    "Driver reported write failure: {}",
                    err_msg
                ))),
                Ok(Err(_recv_err)) => Err(opcua_server::ServerError::Other(
                    "Write confirmation channel closed".into(),
                )),
                Err(_) => Err(opcua_server::ServerError::Other(
                    "Write confirmation timed out".into(),
                )),
            }
        }
        .boxed()
    }
}
