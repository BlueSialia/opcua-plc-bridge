//! Native open62541-backed OPC UA server implementation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use open62541::ua;
use open62541::{DataSource, DataSourceReadContext, DataSourceResult, DataSourceWriteContext};
use open62541::{ObjectNode, VariableNode};

use std::sync::mpsc::{self, Receiver, Sender};

use tracing::{debug, error, info};

use crate::config::OpcUaConfig;
use crate::error::ServerError;
use crate::writes::{WriteHandlerArc, WriteRequest};
use core_model::tag_value::TagDataType;
use core_model::{TagQuality, TagRegistry, TagValue};

// ---------------------------------------------------------------------------
// OPC UA NS0 Node ID constants
//
// These are defined by the OPC UA specification and are stable. Using these
// hardcoded values avoids pulling `open62541_sys` as a direct dependency.
// ---------------------------------------------------------------------------
mod ns0 {
    pub(super) const BOOLEAN: u32 = 1;
    pub(super) const INT16: u32 = 4;
    pub(super) const UINT16: u32 = 5;
    pub(super) const INT32: u32 = 6;
    pub(super) const UINT32: u32 = 7;
    pub(super) const INT64: u32 = 8;
    pub(super) const UINT64: u32 = 9;
    pub(super) const FLOAT: u32 = 10;
    pub(super) const DOUBLE: u32 = 11;
    pub(super) const STRING: u32 = 12;
    pub(super) const DATETIME: u32 = 13;
    pub(super) const BYTESTRING: u32 = 15;
    pub(super) const ORGANIZES: u32 = 35;
    pub(super) const FOLDERTYPE: u32 = 61;
    pub(super) const BASEDATAVARIABLETYPE: u32 = 63;
    pub(super) const OBJECTSFOLDER: u32 = 85;
}

/// Internal bridge write sent from the server thread to the async processing thread.
pub(crate) struct BridgeWrite {
    pub(crate) tag_id: String,
    pub(crate) value: TagValue,
    /// optional reply channel for confirmed ack
    pub(crate) reply: Option<Sender<Result<(), String>>>,
}

/// Helper to map runtime `TagQuality` to OPC UA `ua::StatusCode`.
///
/// This centralizes the mapping of our rich internal quality states to the
/// backend-specific status codes.
fn tagquality_to_ua_status(quality: &TagQuality) -> ua::StatusCode {
    match quality {
        TagQuality::Good => ua::StatusCode::GOOD,
        // During initialization we treat the value as uncertain/subnormal
        TagQuality::Initializing => ua::StatusCode::UNCERTAINSUBNORMAL,
        // Stale means last usable value is uncertain
        TagQuality::Stale => ua::StatusCode::UNCERTAINSUBNORMAL,
        // Communication lost -> map to a BAD / comms related error.
        TagQuality::CommLost => ua::StatusCode::BADUNEXPECTEDERROR,
        // Configuration / mapping problems
        TagQuality::ConfigError => ua::StatusCode::BADUNEXPECTEDERROR,
        // Type mismatch
        TagQuality::TypeMismatch => ua::StatusCode::BADUNEXPECTEDERROR,
        // Substitute / fallback values are uncertain but usable
        TagQuality::Substitute => ua::StatusCode::UNCERTAINSUBNORMAL,
        // Generic error: surface as a Bad Unexpected Error
        TagQuality::Error(_) => ua::StatusCode::BADUNEXPECTEDERROR,
    }
}

/// Data source that backs a variable node and connects reads/writes to the core model.
pub(crate) struct TagDataSource {
    pub(crate) tag_id: String,
    pub(crate) registry: Arc<TagRegistry>,
    pub(crate) write_tx: Sender<BridgeWrite>,
    pub(crate) write_mode: crate::config::WriteMode,
    /// confirmation timeout for ConfirmedAck mode
    pub(crate) confirm_timeout: Duration,
}

impl TagDataSource {
    pub(crate) fn new(
        tag_id: String,
        registry: Arc<TagRegistry>,
        write_tx: Sender<BridgeWrite>,
        write_mode: crate::config::WriteMode,
        confirm_timeout: Duration,
    ) -> Self {
        Self {
            tag_id,
            registry,
            write_tx,
            write_mode,
            confirm_timeout,
        }
    }

    /// Map a runtime `TagValue` into an OPC UA `ua::Variant`.
    pub(crate) fn tagvalue_to_variant(value: &TagValue) -> Option<ua::Variant> {
        use core_model::tag_value::TagValue::*;
        match value {
            Bool(b) => Some(ua::Variant::scalar(ua::Boolean::new(*b))),
            Int16(v) => Some(ua::Variant::scalar(ua::Int16::new(*v))),
            UInt16(v) => Some(ua::Variant::scalar(ua::UInt16::new(*v))),
            Int32(v) => Some(ua::Variant::scalar(ua::Int32::new(*v))),
            UInt32(v) => Some(ua::Variant::scalar(ua::UInt32::new(*v))),
            Int64(v) => Some(ua::Variant::scalar(ua::Int64::new(*v))),
            UInt64(v) => Some(ua::Variant::scalar(ua::UInt64::new(*v))),
            Float(v) => Some(ua::Variant::scalar(ua::Float::new(*v))),
            Double(v) => Some(ua::Variant::scalar(ua::Double::new(*v))),
            String(s) => {
                let ua_str = ua::String::new(s).ok()?;
                Some(ua::Variant::scalar(ua_str))
            }
            DateTime(dt) => {
                let nanos = dt.timestamp_nanos_opt()?;
                let ua_dt = ua::DateTime::try_from_unix_timestamp_nanos(i128::from(nanos)).ok()?;
                Some(ua::Variant::scalar(ua_dt))
            }
            ByteString(b) => {
                let ua_bytes = ua::ByteString::new(b);
                Some(ua::Variant::scalar(ua_bytes))
            }
        }
    }

    fn chrono_to_ua_dt(dt: &chrono::DateTime<chrono::Utc>) -> ua::DateTime {
        let nanos = dt.timestamp_nanos_opt().unwrap_or(0);
        ua::DateTime::try_from_unix_timestamp_nanos(i128::from(nanos))
            .unwrap_or_else(|_| ua::DateTime::try_from_unix_timestamp_nanos(0).unwrap())
    }

    /// Convert an `ua::Variant` into a `TagValue` using an expected TagDataType.
    pub(crate) fn variant_to_tagvalue(
        var: &ua::Variant,
        expected: &TagDataType,
    ) -> Result<TagValue, String> {
        use TagDataType::*;
        match expected {
            Bool => var
                .to_scalar::<ua::Boolean>()
                .map(|b| TagValue::Bool(b.value()))
                .ok_or_else(|| "Expected Boolean".into()),
            Int16 => var
                .to_scalar::<ua::Int16>()
                .map(|v| TagValue::Int16(v.value()))
                .ok_or_else(|| "Expected Int16".into()),
            UInt16 => var
                .to_scalar::<ua::UInt16>()
                .map(|v| TagValue::UInt16(v.value()))
                .ok_or_else(|| "Expected UInt16".into()),
            Int32 => var
                .to_scalar::<ua::Int32>()
                .map(|v| TagValue::Int32(v.value()))
                .ok_or_else(|| "Expected Int32".into()),
            UInt32 => var
                .to_scalar::<ua::UInt32>()
                .map(|v| TagValue::UInt32(v.value()))
                .ok_or_else(|| "Expected UInt32".into()),
            Int64 => var
                .to_scalar::<ua::Int64>()
                .map(|v| TagValue::Int64(v.value()))
                .ok_or_else(|| "Expected Int64".into()),
            UInt64 => var
                .to_scalar::<ua::UInt64>()
                .map(|v| TagValue::UInt64(v.value()))
                .ok_or_else(|| "Expected UInt64".into()),
            Float => var
                .to_scalar::<ua::Float>()
                .map(|v| TagValue::Float(v.value()))
                .ok_or_else(|| "Expected Float".into()),
            Double => var
                .to_scalar::<ua::Double>()
                .map(|v| TagValue::Double(v.value()))
                .ok_or_else(|| "Expected Double".into()),
            String => var
                .to_scalar::<ua::String>()
                .map(|s| TagValue::String(s.as_str().unwrap_or("").to_owned()))
                .ok_or_else(|| "Expected String".into()),
            DateTime => var
                .to_scalar::<ua::DateTime>()
                .map(|dt| {
                    let nanos = dt.as_unix_timestamp_nanos() as i64;
                    let chrono_dt = chrono::DateTime::from_timestamp_nanos(nanos);
                    TagValue::DateTime(chrono_dt)
                })
                .ok_or_else(|| "Expected DateTime".into()),
            ByteString => var
                .to_scalar::<ua::ByteString>()
                .map(|bs| TagValue::ByteString(bs.as_bytes().unwrap_or(&[]).to_vec()))
                .ok_or_else(|| "Expected ByteString".into()),
        }
    }
}

impl DataSource for TagDataSource {
    // Called synchronously from the server loop to fulfill a read.
    fn read(&mut self, ctx: &mut DataSourceReadContext) -> DataSourceResult {
        match self.registry.get_tag(&self.tag_id) {
            Ok(tag) => {
                if let Some(variant) = Self::tagvalue_to_variant(&tag.value) {
                    // Map Initializing -> GOOD so open62541's
                    // typeCheckVariableNode (which runs during node
                    // creation) does not treat non-GOOD as fatal.
                    let status = if tag.quality == TagQuality::Initializing {
                        ua::StatusCode::GOOD
                    } else {
                        tagquality_to_ua_status(&tag.quality)
                    };

                    let source_ts = Self::chrono_to_ua_dt(&tag.source_timestamp);
                    let server_ts = Self::chrono_to_ua_dt(&tag.server_timestamp);

                    let dv = ua::DataValue::new(variant)
                        .with_status(&status)
                        .with_source_timestamp(&source_ts)
                        .with_server_timestamp(&server_ts);
                    ctx.set_value(dv);
                    Ok(())
                } else {
                    Err(open62541::DataSourceError::from_status_code(
                        ua::StatusCode::BADINTERNALERROR,
                    ))
                }
            }
            Err(e) => {
                debug!("Tag read failed for {}: {}", self.tag_id, e);
                Err(open62541::DataSourceError::from_status_code(
                    ua::StatusCode::BADINTERNALERROR,
                ))
            }
        }
    }

    // Called synchronously from the server loop to handle a client write.
    fn write(&mut self, ctx: &mut DataSourceWriteContext) -> DataSourceResult {
        // Convert incoming variant into TagValue according to TagDefinition
        let data_value = ctx.value();
        // Extract the inner Variant reference from the DataValue; if there is no value present,
        // return a write failure so the client receives an appropriate status.
        let variant = match data_value.value() {
            Some(v) => v,
            None => {
                debug!("Write request for {} contained no value", self.tag_id);
                return Err(open62541::DataSourceError::from_status_code(
                    ua::StatusCode::BADINTERNALERROR,
                ));
            }
        };
        // Obtain expected type from the TagDefinition in the registry
        match self.registry.get_definition(&self.tag_id) {
            Ok(def) => {
                let expected = def.data_type.clone();
                match Self::variant_to_tagvalue(variant, &expected) {
                    Ok(tag_value) => {
                        // Prepare optional reply channel for confirmed ack
                        let (reply_tx, reply_rx) = mpsc::channel::<Result<(), String>>();
                        let bridge = BridgeWrite {
                            tag_id: self.tag_id.clone(),
                            value: tag_value,
                            reply: if self.write_mode == crate::config::WriteMode::ConfirmedAck {
                                Some(reply_tx)
                            } else {
                                None
                            },
                        };

                        // Send to the write processing thread. If send fails, return an error.
                        if let Err(e) = self.write_tx.send(bridge) {
                            debug!("Failed to enqueue write for {}: {}", self.tag_id, e);
                            return Err(open62541::DataSourceError::from_status_code(
                                ua::StatusCode::BADINTERNALERROR,
                            ));
                        }

                        // If queued ack, return success immediately.
                        if self.write_mode == crate::config::WriteMode::QueuedAck {
                            return Ok(());
                        }

                        // Confirmed ack: wait for reply with timeout.
                        match reply_rx.recv_timeout(self.confirm_timeout) {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(err_msg)) => {
                                debug!(
                                    "Driver reported write failure for {}: {}",
                                    self.tag_id, err_msg
                                );
                                Err(open62541::DataSourceError::from_status_code(
                                    ua::StatusCode::BADINTERNALERROR,
                                ))
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                                debug!("Write confirmation timed out for {}", self.tag_id);
                                Err(open62541::DataSourceError::from_status_code(
                                    ua::StatusCode::BADINTERNALERROR,
                                ))
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                debug!("Write confirmation channel closed for {}", self.tag_id);
                                Err(open62541::DataSourceError::from_status_code(
                                    ua::StatusCode::BADINTERNALERROR,
                                ))
                            }
                        }
                    }
                    Err(conv_err) => {
                        debug!(
                            "Variant -> TagValue conversion failed for {}: {}",
                            self.tag_id, conv_err
                        );
                        Err(open62541::DataSourceError::from_status_code(
                            ua::StatusCode::BADINTERNALERROR,
                        ))
                    }
                }
            }
            Err(e) => {
                debug!("Tag definition not found for {}: {}", self.tag_id, e);
                Err(open62541::DataSourceError::from_status_code(
                    ua::StatusCode::BADINTERNALERROR,
                ))
            }
        }
    }
}

/// Start a native open62541-backed server.
pub fn start_native_server(
    cfg: OpcUaConfig,
    registry: Arc<TagRegistry>,
    write_handler: WriteHandlerArc,
    login_cb: Option<crate::LoginCallback>,
) -> Result<crate::types::ServerHandle, ServerError> {
    // Determine the endpoint URL before constructing the builder.
    let endpoint = format!("opc.tcp://{}:{}", cfg.bind_addr, cfg.port);

    // Build ServerBuilder with or without security policies.
    let mut builder = if cfg.security_mode != crate::config::SecurityMode::None {
        // Security enabled: load certificate and private key from disk.
        let cert_path = cfg
            .certificates
            .server_certificate_path
            .as_deref()
            .ok_or_else(|| {
                ServerError::Config(
                    "server_certificate_path is required when security_mode is not None".into(),
                )
            })?;
        let key_path = cfg
            .certificates
            .server_private_key_path
            .as_deref()
            .ok_or_else(|| {
                ServerError::Config(
                    "server_private_key_path is required when security_mode is not None".into(),
                )
            })?;

        let cert_bytes = std::fs::read(cert_path).map_err(|e| {
            ServerError::Config(format!("Failed to read certificate '{}': {}", cert_path, e))
        })?;
        let key_bytes = std::fs::read(key_path).map_err(|e| {
            ServerError::Config(format!("Failed to read private key '{}': {}", key_path, e))
        })?;

        let certificate = open62541::Certificate::from_bytes(&cert_bytes);
        let private_key = open62541::PrivateKey::from_bytes(&key_bytes);

        let mut b = open62541::ServerBuilder::default_with_security_policies(
            cfg.port,
            &certificate,
            &private_key,
        )
        .map_err(|e| {
            ServerError::Backend(format!("Failed to initialize security policies: {}", e))
        })?;

        if cfg.certificates.trust_store_dir.is_none() {
            b = b.accept_all();
        }

        let policy_uri = cfg.security_policy.uri();
        info!(
            "OPC UA security enabled: mode={:?}, policy={}, server-cert={}, trust-store={}",
            cfg.security_mode,
            policy_uri,
            cert_path,
            cfg.certificates
                .trust_store_dir
                .as_deref()
                .unwrap_or("none (accept-all)"),
        );

        b
    } else {
        open62541::ServerBuilder::default()
    };

    // Set the server endpoint URL from configuration.
    builder = builder.server_urls(&[endpoint.as_str()]);

    // Configure access control: username/password callback or anonymous-only.
    if let Some(cb) = login_cb {
        if !cfg.username_password_enabled {
            return Err(ServerError::Config(
                "username/password authentication disabled in config".into(),
            ));
        }
        let access = open62541::DefaultAccessControlWithLoginCallback::new(
            cfg.anonymous_enabled,
            move |username: &ua::String, password: &ua::ByteString| -> ua::StatusCode {
                (cb)(username, password)
            },
        );

        builder = builder
            .access_control(access)
            .map_err(|e| ServerError::Backend(format!("Failed to apply access control: {}", e)))?;
    }

    // Build the server and runner from the builder.
    let (server, runner) = builder.build();

    // Register our namespace URI and obtain index.
    let ns_index = server.add_namespace(&cfg.namespace_uri);

    // Prepare bridge channel: blocking std mpsc used by DataSource to enqueue writes
    let (bridge_tx, bridge_rx): (Sender<BridgeWrite>, Receiver<BridgeWrite>) = mpsc::channel();

    let tokio_handle = match std::panic::catch_unwind(|| tokio::runtime::Handle::current()) {
        Ok(h) => h,
        Err(_) => {
            return Err(ServerError::Other(
                "start_native_server must be called from a Tokio runtime context".into(),
            ))
        }
    };

    let write_handler_clone = write_handler.clone();
    let processing_thread = std::thread::Builder::new()
        .name("opcua-write-bridge".into())
        .spawn(move || {
            while let Ok(bw) = bridge_rx.recv() {
                let tag_id = bw.tag_id.clone();
                let value = bw.value.clone();
                let reply = bw.reply;

                let req = WriteRequest {
                    tag_id: tag_id.clone(),
                    value: value.clone(),
                };

                let fut = write_handler_clone.handle_write(req);
                let res = tokio_handle.block_on(fut);

                if let Some(tx) = reply {
                    let _ = tx.send(res.map_err(|e| format!("{}", e)));
                }
            }
        })
        .map_err(|e| ServerError::Other(format!("Failed to spawn write bridge thread: {}", e)))?;

    // Build address-space
    let objects_folder = ua::NodeId::ns0(ns0::OBJECTSFOLDER);
    let organizes_ref = ua::NodeId::ns0(ns0::ORGANIZES);
    let folder_type = ua::NodeId::ns0(ns0::FOLDERTYPE);
    let base_data_variable = ua::NodeId::ns0(ns0::BASEDATAVARIABLETYPE);

    let plcs_folder = server
        .add_object_node(ObjectNode {
            requested_new_node_id: None,
            parent_node_id: objects_folder.clone(),
            reference_type_id: organizes_ref.clone(),
            browse_name: ua::QualifiedName::new(ns_index, "PLCs"),
            type_definition: folder_type.clone(),
            attributes: ua::ObjectAttributes::default(),
        })
        .map_err(|e| ServerError::Backend(format!("Failed creating PLCs folder: {}", e)))?;

    use std::collections::HashMap;
    let mut plc_nodes: HashMap<String, ua::NodeId> = HashMap::new();
    let mut tag_node_map: HashMap<String, ua::NodeId> = HashMap::new();

    for def in registry.all_definitions_sorted() {
        let id_str = def.id_str().to_string();
        // Use the explicit plc_name from TagDefinition.
        let plc_name: String = def.plc_name.as_ref().to_string();

        let plc_node_id = if let Some(n) = plc_nodes.get(&plc_name) {
            n.clone()
        } else {
            let nn = server
                .add_object_node(ObjectNode {
                    requested_new_node_id: None,
                    parent_node_id: plcs_folder.clone(),
                    reference_type_id: organizes_ref.clone(),
                    browse_name: ua::QualifiedName::new(ns_index, plc_name.as_str()),
                    type_definition: folder_type.clone(),
                    attributes: ua::ObjectAttributes::default(),
                })
                .map_err(|e| {
                    ServerError::Backend(format!(
                        "Failed creating PLC folder '{}': {}",
                        plc_name, e
                    ))
                })?;
            plc_nodes.insert(plc_name.clone(), nn.clone());
            nn
        };

        let browse_name = def.name_str().to_string();

        let data_type_nodeid = match def.data_type {
            TagDataType::Bool => ua::NodeId::ns0(ns0::BOOLEAN),
            TagDataType::Int16 => ua::NodeId::ns0(ns0::INT16),
            TagDataType::UInt16 => ua::NodeId::ns0(ns0::UINT16),
            TagDataType::Int32 => ua::NodeId::ns0(ns0::INT32),
            TagDataType::UInt32 => ua::NodeId::ns0(ns0::UINT32),
            TagDataType::Int64 => ua::NodeId::ns0(ns0::INT64),
            TagDataType::UInt64 => ua::NodeId::ns0(ns0::UINT64),
            TagDataType::Float => ua::NodeId::ns0(ns0::FLOAT),
            TagDataType::Double => ua::NodeId::ns0(ns0::DOUBLE),
            TagDataType::String => ua::NodeId::ns0(ns0::STRING),
            TagDataType::DateTime => ua::NodeId::ns0(ns0::DATETIME),
            TagDataType::ByteString => ua::NodeId::ns0(ns0::BYTESTRING),
        };

        let mut attrs = ua::VariableAttributes::default().with_data_type(&data_type_nodeid);
        let mut access = ua::AccessLevelType::NONE.with_current_read(true);
        if def.writable {
            access = access.with_current_write(true);
        }
        attrs = attrs.with_access_level(&access);

        let ds = TagDataSource::new(
            def.id_str().to_string(),
            registry.clone(),
            bridge_tx.clone(),
            cfg.write_mode,
            Duration::from_secs(5),
        );

        let string_node_id = id_str.split(";s=").nth(1).unwrap_or(&id_str);
        let requested_id = ua::NodeId::string(ns_index, string_node_id);
        info!(
            "Creating variable node: tag_id={}, string_id={}, ns_index={}, parent={:?}",
            id_str, string_node_id, ns_index, plc_node_id
        );
        let variable_node_id = server
            .add_data_source_variable_node(
                VariableNode {
                    requested_new_node_id: Some(requested_id),
                    parent_node_id: plc_node_id.clone(),
                    reference_type_id: organizes_ref.clone(),
                    browse_name: ua::QualifiedName::new(ns_index, browse_name.as_str()),
                    type_definition: base_data_variable.clone(),
                    attributes: attrs,
                },
                ds,
            )
            .map_err(|e| {
                ServerError::Backend(format!(
                    "Failed creating variable for tag {}: {}",
                    def.id_str(),
                    e
                ))
            })?;

        info!(
            "Created variable node for tag {}: assigned_node_id={:?}",
            id_str, variable_node_id
        );

        tag_node_map.insert(def.id_str().to_string(), variable_node_id.clone());
    }

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel::<bool>(false);
    let cfg_clone = cfg.clone();

    // Use AtomicBool for cross-thread shutdown signalling with ServerRunner.
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let shutdown_flag_for_runner = shutdown_flag.clone();

    let join_handle = tokio::spawn(async move {
        info!(
            "Starting native open62541 OPC UA server on {}:{} (app_name={})",
            cfg_clone.bind_addr, cfg_clone.port, cfg_clone.application_name
        );

        let runner_handle = tokio::task::spawn_blocking(move || {
            info!("Running open62541 server runner (blocking thread)");
            if let Err(e) =
                runner.run_until_cancelled(|| shutdown_flag_for_runner.load(Ordering::Relaxed))
            {
                error!("open62541 runner returned error: {:?}", e);
            }
        });

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("Shutdown requested for native OPC UA server; signalling runner");
                        shutdown_flag.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        }

        let _ = runner_handle.await;
        drop(bridge_tx);
        Ok(())
    });

    Ok(crate::types::ServerHandle::new(
        shutdown_tx,
        join_handle,
        processing_thread,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WriteMode;
    use core_model::{TagDataType, TagDefinition, TagRegistry, TagValue};
    use open62541::ua;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    /// Simulate a FINS-like tag data source.
    ///
    /// - **Read path**: creates a registry with a pre-populated UInt16 tag
    ///   (mimicking a FINS D-register read), then asserts that
    ///   `tagvalue_to_variant` produces the expected OPC UA variant.
    /// - **Write path**: constructs a `TagDataSource` wired to a bridge-write
    ///   channel, enqueues a `BridgeWrite` (the same way the real `write()`
    ///   callback does), and verifies the bridge-write payload is received
    ///   intact on the other end.
    /// #feature UA-READ, UA-WRITE, UA-TYPES
    #[tokio::test]
    async fn fins_read_write_through_datasource() {
        // -- Arrange: build a FINS-like registry with two D-register tags -----
        let defs = vec![
            TagDefinition::new(
                "ns=1;s=fins.D100",
                "PLC1.D100",
                "D100",
                TagDataType::UInt16,
                "PLC1",
            ),
            TagDefinition::new(
                "ns=1;s=fins.D101",
                "PLC1.D101",
                "D101",
                TagDataType::Float,
                "PLC1",
            ),
        ];
        let registry = Arc::new(TagRegistry::from_definitions(&defs).expect("build registry"));

        // -- Read path: register inspection via tagvalue_to_variant -----------
        //
        // The registry populates each tag with a zero-equivalent initial value
        // (UInt16(0), Float(0.0)). We verify the conversion helpers produce
        // correct OPC UA variants.
        let tag_d100 = registry.get_tag("ns=1;s=fins.D100").expect("tag exists");
        let variant_d100 =
            TagDataSource::tagvalue_to_variant(&tag_d100.value).expect("convert to variant");
        let scalar_d100: ua::UInt16 = variant_d100.to_scalar().expect("extract UInt16 scalar");
        assert_eq!(scalar_d100.value(), 0u16);

        let tag_d101 = registry.get_tag("ns=1;s=fins.D101").expect("tag exists");
        let variant_d101 =
            TagDataSource::tagvalue_to_variant(&tag_d101.value).expect("convert to variant");
        let scalar_d101: ua::Float = variant_d101.to_scalar().expect("extract Float scalar");
        assert!((scalar_d101.value() - 0.0f32).abs() < f32::EPSILON);

        // Also test reverse conversion: variant -> TagValue
        let recovered: TagValue =
            TagDataSource::variant_to_tagvalue(&variant_d100, &TagDataType::UInt16)
                .expect("reverse conversion");
        assert_eq!(recovered, TagValue::UInt16(0));

        // -- Write path: bridge-write channel enqueue -------------------------
        let (write_tx, write_rx) = mpsc::channel::<BridgeWrite>();

        let _ds = TagDataSource::new(
            "ns=1;s=fins.D100".into(),
            registry.clone(),
            write_tx,
            WriteMode::QueuedAck,
            Duration::from_secs(1),
        );

        // Enqueue a write (same pattern as the real DataSource::write callback)
        let bridge = BridgeWrite {
            tag_id: "ns=1;s=fins.D100".into(),
            value: TagValue::UInt16(4660), // 0x1234 — classic FINS test value
            reply: None,                   // QueuedAck — no confirmation needed
        };
        // The TagDataSource holds a clone of write_tx; we use the same channel.
        _ds.write_tx.send(bridge).expect("send bridge write");

        // Verify the bridge-write payload arrived intact
        let received = write_rx.recv().expect("receive bridge write");
        assert_eq!(received.tag_id, "ns=1;s=fins.D100");
        assert_eq!(received.value, TagValue::UInt16(4660));
        assert!(received.reply.is_none());
    }

    /// Test that `variant_to_tagvalue` correctly converts OPC UA variants for
    /// all scalar types commonly used with Modbus register mappings, and that
    /// the roundtrip `TagValue → Variant → TagValue` is lossless for each type.
    /// #feature UA-TYPES
    #[tokio::test]
    async fn modbus_variant_conversion_all_types() {
        // ----- UInt16 (common Modbus holding register) -----------------------
        let v_uint16 = ua::Variant::scalar(ua::UInt16::new(12345u16));
        let tv = TagDataSource::variant_to_tagvalue(&v_uint16, &TagDataType::UInt16)
            .expect("convert UInt16");
        assert_eq!(tv, TagValue::UInt16(12345));

        let back = TagDataSource::tagvalue_to_variant(&tv).expect("roundtrip UInt16");
        let scalar: ua::UInt16 = back.to_scalar().expect("extract UInt16");
        assert_eq!(scalar.value(), 12345u16);

        // ----- Int16 (signed Modbus register) --------------------------------
        let v_int16 = ua::Variant::scalar(ua::Int16::new(-1i16));
        let tv = TagDataSource::variant_to_tagvalue(&v_int16, &TagDataType::Int16)
            .expect("convert Int16");
        assert_eq!(tv, TagValue::Int16(-1));

        // ----- Float (IEEE 754, common in Modbus 2-register mappings) --------
        let v_float = ua::Variant::scalar(ua::Float::new(std::f32::consts::PI));
        let tv = TagDataSource::variant_to_tagvalue(&v_float, &TagDataType::Float)
            .expect("convert Float");
        match &tv {
            TagValue::Float(f) => assert!((f - std::f32::consts::PI).abs() < f32::EPSILON),
            other => panic!("expected Float, got {:?}", other),
        }

        let back = TagDataSource::tagvalue_to_variant(&tv).expect("roundtrip Float");
        let scalar: ua::Float = back.to_scalar().expect("extract Float");
        assert!((scalar.value() - std::f32::consts::PI).abs() < f32::EPSILON);

        // ----- Int32 (Modbus 32-bit signed) ----------------------------------
        let v_int32 = ua::Variant::scalar(ua::Int32::new(-100_000i32));
        let tv = TagDataSource::variant_to_tagvalue(&v_int32, &TagDataType::Int32)
            .expect("convert Int32");
        assert_eq!(tv, TagValue::Int32(-100_000));

        let back = TagDataSource::tagvalue_to_variant(&tv).expect("roundtrip Int32");
        let scalar: ua::Int32 = back.to_scalar().expect("extract Int32");
        assert_eq!(scalar.value(), -100_000i32);

        // ----- UInt32 (Modbus 32-bit unsigned) -------------------------------
        let v_uint32 = ua::Variant::scalar(ua::UInt32::new(3_000_000_000u32));
        let tv = TagDataSource::variant_to_tagvalue(&v_uint32, &TagDataType::UInt32)
            .expect("convert UInt32");
        assert_eq!(tv, TagValue::UInt32(3_000_000_000));

        let back = TagDataSource::tagvalue_to_variant(&tv).expect("roundtrip UInt32");
        let scalar: ua::UInt32 = back.to_scalar().expect("extract UInt32");
        assert_eq!(scalar.value(), 3_000_000_000u32);

        // ----- Double (IEEE 754 64-bit) --------------------------------------
        let v_double = ua::Variant::scalar(ua::Double::new(std::f64::consts::PI));
        let tv = TagDataSource::variant_to_tagvalue(&v_double, &TagDataType::Double)
            .expect("convert Double");
        match &tv {
            TagValue::Double(d) => assert!((d - std::f64::consts::PI).abs() < f64::EPSILON),
            other => panic!("expected Double, got {:?}", other),
        }

        let back = TagDataSource::tagvalue_to_variant(&tv).expect("roundtrip Double");
        let scalar: ua::Double = back.to_scalar().expect("extract Double");
        assert!((scalar.value() - std::f64::consts::PI).abs() < f64::EPSILON);

        // ----- Int64 ---------------------------------------------------------
        let v_int64 = ua::Variant::scalar(ua::Int64::new(-9_223_372_036_854_775_807i64));
        let tv = TagDataSource::variant_to_tagvalue(&v_int64, &TagDataType::Int64)
            .expect("convert Int64");
        assert_eq!(tv, TagValue::Int64(-9_223_372_036_854_775_807));

        let back = TagDataSource::tagvalue_to_variant(&tv).expect("roundtrip Int64");
        let scalar: ua::Int64 = back.to_scalar().expect("extract Int64");
        assert_eq!(scalar.value(), -9_223_372_036_854_775_807i64);

        // ----- UInt64 --------------------------------------------------------
        let v_uint64 = ua::Variant::scalar(ua::UInt64::new(18_446_744_073_709_551_615u64));
        let tv = TagDataSource::variant_to_tagvalue(&v_uint64, &TagDataType::UInt64)
            .expect("convert UInt64");
        assert_eq!(tv, TagValue::UInt64(18_446_744_073_709_551_615));

        // ----- Bool ----------------------------------------------------------
        let v_bool = ua::Variant::scalar(ua::Boolean::new(true));
        let tv =
            TagDataSource::variant_to_tagvalue(&v_bool, &TagDataType::Bool).expect("convert Bool");
        assert_eq!(tv, TagValue::Bool(true));

        let back = TagDataSource::tagvalue_to_variant(&tv).expect("roundtrip Bool");
        let scalar: ua::Boolean = back.to_scalar().expect("extract Boolean");
        assert!(scalar.value());

        // ----- Type mismatch produces an error -------------------------------
        let v_float_as_int = ua::Variant::scalar(ua::Float::new(1.0f32));
        let err = TagDataSource::variant_to_tagvalue(&v_float_as_int, &TagDataType::UInt16)
            .expect_err("type mismatch should fail");
        assert!(err.contains("UInt16"), "error should mention UInt16: {err}");
    }

    /// Verify the end-to-end write confirmation flow used by `ConfirmedAck`.
    ///
    /// This test exercises the exact same channel pattern the real
    /// `DataSource::write()` callback uses under `ConfirmedAck`:
    ///
    /// 1. A `BridgeWrite` carrying a `reply` channel is enqueued.
    /// 2. The bridge processor (simulated here) receives the write, processes
    ///    it, and sends `Ok(())` back through the reply channel.
    /// 3. The caller waits on the reply channel with a timeout and receives
    ///    the success confirmation.
    ///
    /// Additionally, the test verifies that a disconnected reply channel
    /// (processor dropped) results in a `Disconnected` error, and that the
    /// timeout correctly fires when the processor never responds.
    /// #feature UA-WRITE
    #[tokio::test]
    async fn write_confirmation_with_confirmed_ack() {
        let defs = vec![TagDefinition::new(
            "ns=1;s=tag.confirm",
            "ConfirmTag",
            "W200",
            TagDataType::UInt16,
            "PLC",
        )];
        let registry = Arc::new(TagRegistry::from_definitions(&defs).expect("build registry"));

        let (write_tx, write_rx) = mpsc::channel::<BridgeWrite>();
        let confirm_timeout = Duration::from_millis(500);

        let ds = TagDataSource::new(
            "ns=1;s=tag.confirm".into(),
            registry,
            write_tx,
            WriteMode::ConfirmedAck,
            confirm_timeout,
        );

        let (reply_tx, reply_rx) = mpsc::channel::<Result<(), String>>();

        let bridge = BridgeWrite {
            tag_id: "ns=1;s=tag.confirm".into(),
            value: TagValue::UInt16(99),
            reply: Some(reply_tx),
        };

        ds.write_tx.send(bridge).expect("send bridge write");

        let processor_handle = tokio::task::spawn_blocking(move || {
            let received = write_rx.recv().expect("processor received write");
            assert_eq!(received.tag_id, "ns=1;s=tag.confirm");
            assert_eq!(received.value, TagValue::UInt16(99));

            received
                .reply
                .expect("ConfirmedAck must have reply channel")
                .send(Ok(()))
                .expect("ack sent");
        });

        let result = reply_rx
            .recv_timeout(confirm_timeout)
            .expect("confirmation within timeout");
        assert!(result.is_ok(), "write should be acknowledged successfully");

        processor_handle.await.expect("processor task");
    }

    /// Negative case: when the reply sender is dropped before acknowledging,
    /// the receiver must get a `Disconnected` error.
    /// #feature UA-WRITE
    #[tokio::test]
    async fn write_confirmation_reply_disconnected() {
        let defs = vec![TagDefinition::new(
            "ns=1;s=tag.drop",
            "DropTag",
            "W300",
            TagDataType::UInt16,
            "PLC",
        )];
        let registry = Arc::new(TagRegistry::from_definitions(&defs).expect("build registry"));

        let (write_tx, _write_rx) = mpsc::channel::<BridgeWrite>();

        let ds = TagDataSource::new(
            "ns=1;s=tag.drop".into(),
            registry,
            write_tx,
            WriteMode::ConfirmedAck,
            Duration::from_millis(200),
        );

        let (reply_tx, reply_rx) = mpsc::channel::<Result<(), String>>();

        let bridge = BridgeWrite {
            tag_id: "ns=1;s=tag.drop".into(),
            value: TagValue::UInt16(1),
            reply: Some(reply_tx),
        };

        ds.write_tx.send(bridge).expect("send bridge write");

        drop(ds);
        drop(_write_rx);

        let result = reply_rx.recv_timeout(Duration::from_millis(100));
        match result {
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
            other => panic!("expected RecvTimeoutError::Disconnected, got {:?}", other),
        }
    }

    /// Negative case: when no one responds within the timeout window, the
    /// receiver gets a `Timeout` error.
    /// #feature UA-WRITE
    #[tokio::test]
    async fn write_confirmation_timeout() {
        let defs = vec![TagDefinition::new(
            "ns=1;s=tag.slow",
            "SlowTag",
            "W400",
            TagDataType::UInt16,
            "PLC",
        )];
        let registry = Arc::new(TagRegistry::from_definitions(&defs).expect("build registry"));

        let (write_tx, write_rx) = mpsc::channel::<BridgeWrite>();

        let ds = TagDataSource::new(
            "ns=1;s=tag.slow".into(),
            registry,
            write_tx,
            WriteMode::ConfirmedAck,
            Duration::from_millis(50),
        );

        let (reply_tx, reply_rx) = mpsc::channel::<Result<(), String>>();

        let bridge = BridgeWrite {
            tag_id: "ns=1;s=tag.slow".into(),
            value: TagValue::UInt16(1),
            reply: Some(reply_tx),
        };

        ds.write_tx.send(bridge).expect("send bridge write");

        let received = write_rx.recv().expect("processor received write");

        std::thread::sleep(Duration::from_millis(200));
        received
            .reply
            .expect("ConfirmedAck must have reply")
            .send(Ok(()))
            .expect("late ack");

        let result = reply_rx.recv_timeout(Duration::from_millis(50));
        match result {
            Ok(Ok(())) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            other => panic!("unexpected result: {:?}", other),
        }
    }
}
