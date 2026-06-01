//! Runtime entrypoint and orchestrator.
//!
//! Startup sequence:
//! 1. Parse CLI args and load the runtime configuration (TOML or YAML).
//! 2. Populate the `TagRegistry` from config-defined tags.
//! 3. Build protocol drivers (`FinsDriver` / `ModbusDriver`) per PLC config.
//! 4. Wire up write routing (tag → driver sender) for OPC UA write forwarding.
//! 5. Start a health-event drain loop.
//! 6. Start the OPC UA server.
//! 7. Start all driver pollers.
//! 8. Wait for SIGTERM / Ctrl+C and perform graceful shutdown.

#![warn(missing_docs)]

use std::collections::HashSet;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use tokio::net::lookup_host;
use tokio::signal;
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

use core_model::{TagDefinition, TagRegistry};

use driver_fins::{FinsConfig as FinsConfigInternal, FinsDriver};
use driver_modbus::{ModbusConfig as ModbusConfigInternal, ModbusDriver};

use opcua_server::{
    CertificateConfig, OpcUaConfig, SecurityMode, SecurityPolicy, Server, ServerHandle,
};

use dashmap::DashMap;
use serde_json::Value as JsonValue;

mod cli;
mod config;
mod runtime_driver;
mod tags;
mod write_handler;

use cli::Cli;
use config::{
    parse_security_mode, parse_security_policy, parse_write_mode, PlcConfig, RuntimeConfig,
};
use tags::tagconfig_to_definition;
use tags::{tagconfig_to_fins_mapping, tagconfig_to_modbus_mapping};
use write_handler::RuntimeWriteHandler;

/// Resolve a PLC endpoint string to a `SocketAddr`.
async fn resolve_endpoint(host_port: &str, plc_name: &str) -> Result<SocketAddr> {
    lookup_host(host_port)
        .await
        .with_context(|| format!("Failed to resolve endpoint {} for {}", host_port, plc_name))?
        .next()
        .ok_or_else(|| anyhow!("No addresses found for {}", host_port))
}

/// Set up a FINS protocol driver for the given PLC config and wire it into the runtime.
async fn setup_fins_driver(
    plc: &PlcConfig,
    registry: &Arc<TagRegistry>,
    write_handler: &mut RuntimeWriteHandler,
    health_tx: &mpsc::Sender<JsonValue>,
    drivers: &mut Vec<Arc<runtime_driver::RuntimeDriver>>,
) -> Result<()> {
    let endpoint = resolve_endpoint(&plc.endpoint, &plc.name).await?;

    let mut mappings = Vec::new();
    for t in &plc.tags {
        let m = tagconfig_to_fins_mapping(t)
            .with_context(|| format!("FINS mapping error for {}::{}", plc.name, t.id))?;
        mappings.push(m);
    }

    let fins_cfg = FinsConfigInternal {
        name: plc.name.clone(),
        endpoint,
        cycle_ms: plc.cycle_ms,
        keepalive_secs: plc.keepalive_secs.unwrap_or(30),
        max_backoff_secs: plc.max_backoff_secs.unwrap_or(30),
        mappings,
        max_words_per_request: plc.max_words_per_request.unwrap_or(960),
    };
    let fins_drv = Arc::new(FinsDriver::new(fins_cfg, registry.clone()));

    driver_common::ProtocolDriver::validate(fins_drv.as_ref())
        .map_err(|e| anyhow!("FINS driver validation failed for {}: {}", plc.name, e))?;

    let sender = fins_drv.write_sender();
    let _ = fins_drv.set_health_sender(health_tx.clone()).await;
    for t in &plc.tags {
        let full_id: Arc<str> = Arc::from(t.id.clone());
        write_handler.add_route_for_fins(full_id.clone(), sender.clone());
    }

    let proto: Arc<dyn driver_common::ProtocolDriver> = fins_drv.clone();
    let mut runtime_drv = runtime_driver::RuntimeDriver::new(
        proto,
        plc.name.clone(),
        Duration::from_millis(plc.cycle_ms),
        registry.clone(),
    );
    runtime_drv.set_health_sender(health_tx.clone());
    let runtime_drv = Arc::new(runtime_drv);
    runtime_drv.start().await;
    drivers.push(runtime_drv);

    Ok(())
}

/// Set up a Modbus protocol driver for the given PLC config and wire it into the runtime.
async fn setup_modbus_driver(
    plc: &PlcConfig,
    registry: &Arc<TagRegistry>,
    write_handler: &mut RuntimeWriteHandler,
    health_tx: &mpsc::Sender<JsonValue>,
    drivers: &mut Vec<Arc<runtime_driver::RuntimeDriver>>,
) -> Result<()> {
    let endpoint = resolve_endpoint(&plc.endpoint, &plc.name).await?;

    let mut mappings = Vec::new();
    for t in &plc.tags {
        let m = tagconfig_to_modbus_mapping(t)
            .with_context(|| format!("Modbus mapping error for {}::{}", plc.name, t.id))?;
        mappings.push(m);
    }

    let modbus_cfg = ModbusConfigInternal {
        name: plc.name.clone(),
        endpoint,
        unit_id: plc.unit_id.unwrap_or(1),
        cycle_ms: plc.cycle_ms,
        mappings,
        keepalive_secs: plc.keepalive_secs.unwrap_or(30),
        max_backoff_secs: plc.max_backoff_secs.unwrap_or(30),
        io_timeout_ms: plc.io_timeout_ms.unwrap_or(2000),
    };
    let modbus_drv = Arc::new(ModbusDriver::new(modbus_cfg, registry.clone()));

    driver_common::ProtocolDriver::validate(modbus_drv.as_ref())
        .map_err(|e| anyhow!("Modbus driver validation failed for {}: {}", plc.name, e))?;

    let sender = modbus_drv.write_sender();
    let _ = modbus_drv.set_health_sender(health_tx.clone()).await;
    for t in &plc.tags {
        let full_id: Arc<str> = Arc::from(t.id.clone());
        write_handler.add_route_for_modbus(full_id.clone(), sender.clone());
    }

    let proto: Arc<dyn driver_common::ProtocolDriver> = modbus_drv.clone();
    let mut runtime_drv = runtime_driver::RuntimeDriver::new(
        proto,
        plc.name.clone(),
        Duration::from_millis(plc.cycle_ms),
        registry.clone(),
    );
    runtime_drv.set_health_sender(health_tx.clone());
    let runtime_drv = Arc::new(runtime_drv);
    runtime_drv.start().await;
    drivers.push(runtime_drv);

    Ok(())
}

/// Build the internal `OpcUaConfig` from runtime config, with strict validation.
fn build_opcua_config(
    opcua_cfg: &config::OpcUaConfig,
    write_mode: opcua_server::WriteMode,
    security_mode: SecurityMode,
    security_policy: SecurityPolicy,
) -> OpcUaConfig {
    OpcUaConfig {
        bind_addr: opcua_cfg.bind_addr.clone(),
        port: opcua_cfg.port,
        application_name: opcua_cfg.application_name.clone(),
        application_uri: opcua_cfg.application_uri.clone(),
        namespace_uri: opcua_cfg.namespace_uri.clone(),
        anonymous_enabled: opcua_cfg.anonymous_enabled,
        username_password_enabled: opcua_cfg.username_password_enabled,
        max_sessions: opcua_cfg.max_sessions,
        max_subscriptions: opcua_cfg.max_subscriptions,
        write_mode,
        security_mode,
        security_policy,
        certificates: CertificateConfig {
            server_certificate_path: opcua_cfg.server_certificate_path.clone(),
            server_private_key_path: opcua_cfg.server_private_key_path.clone(),
            trust_store_dir: opcua_cfg.trust_store_dir.clone(),
            reject_store_dir: opcua_cfg.reject_store_dir.clone(),
            min_key_length: opcua_cfg.min_key_length.unwrap_or(2048),
        },
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let cli = Cli::parse();
    info!("Starting runtime with config: {:?}", cli.config);

    let cfg_text = fs::read_to_string(&cli.config)
        .with_context(|| format!("Failed reading config {}", cli.config.display()))?;
    let cfg: RuntimeConfig = config::load_runtime_config(&cli.config, &cfg_text)
        .map_err(|e| anyhow!("Config parse error: {}", e))?;

    let mut all_definitions: Vec<TagDefinition> = Vec::new();
    for plc in &cfg.plcs {
        for t in &plc.tags {
            let def = tagconfig_to_definition(t, &plc.name).with_context(|| {
                format!("Failed to convert tag config for {}::{}", plc.name, t.id)
            })?;
            all_definitions.push(def);
        }
    }

    let registry = Arc::new(
        TagRegistry::from_definitions(&all_definitions)
            .map_err(|e| anyhow!("Failed building TagRegistry: {}", e))?,
    );
    info!(
        "Populated TagRegistry with {} tag definitions",
        all_definitions.len()
    );

    let health_capacity = cfg.health_channel_capacity.unwrap_or(128);
    let drv_write_send_timeout_ms = cfg.driver_write_send_timeout_ms.unwrap_or(2000);
    let drv_write_send_timeout = Duration::from_millis(drv_write_send_timeout_ms);
    let write_confirm_timeout_ms = cfg.write_confirm_timeout_ms.unwrap_or(5000);
    let write_confirm_timeout = Duration::from_millis(write_confirm_timeout_ms);

    let mut write_handler = RuntimeWriteHandler::new(registry.clone(), write_confirm_timeout)
        .with_driver_send_timeout(drv_write_send_timeout);

    let health_dash: Arc<DashMap<String, JsonValue>> = Arc::new(DashMap::new());
    let (health_tx, mut health_rx) = mpsc::channel::<JsonValue>(health_capacity);

    let known_plcs: Arc<HashSet<String>> =
        Arc::new(cfg.plcs.iter().map(|p| p.name.clone()).collect());
    let _health_task = {
        let hm_dash = health_dash.clone();
        let known_plcs = known_plcs.clone();
        tokio::spawn(async move {
            while let Some(evt) = health_rx.recv().await {
                if let Some(plc) = evt.get("plc").and_then(|v| v.as_str()) {
                    if known_plcs.contains(plc) {
                        hm_dash.insert(plc.to_string(), evt);
                    }
                }
                while let Ok(evt) = health_rx.try_recv() {
                    if let Some(plc) = evt.get("plc").and_then(|v| v.as_str()) {
                        if known_plcs.contains(plc) {
                            hm_dash.insert(plc.to_string(), evt);
                        }
                    }
                }
            }
        })
    };

    let mut drivers: Vec<Arc<runtime_driver::RuntimeDriver>> = Vec::new();

    for plc in cfg.plcs {
        match plc.protocol.to_lowercase().as_str() {
            "fins" => {
                setup_fins_driver(
                    &plc,
                    &registry,
                    &mut write_handler,
                    &health_tx,
                    &mut drivers,
                )
                .await?;
            }
            "modbus" => {
                setup_modbus_driver(
                    &plc,
                    &registry,
                    &mut write_handler,
                    &health_tx,
                    &mut drivers,
                )
                .await?;
            }
            other => {
                warn!(
                    "Unsupported protocol '{}' for PLC '{}'; skipping",
                    other, plc.name
                );
            }
        }
    }

    let opcua_cfg = cfg.opcua;

    let write_mode = parse_write_mode(&opcua_cfg.write_mode).map_err(|e| anyhow!("{}", e))?;
    let security_mode =
        parse_security_mode(opcua_cfg.security_mode.as_deref()).map_err(|e| anyhow!("{}", e))?;
    let security_policy = parse_security_policy(opcua_cfg.security_policy.as_deref())
        .map_err(|e| anyhow!("{}", e))?;

    if security_mode == SecurityMode::None && security_policy == SecurityPolicy::None {
        warn!(
            "OPC UA server starting with NO SECURITY (anonymous={}). \
             This is acceptable for local development or air-gapped networks. \
             For production, set security_mode and security_policy in the config.",
            opcua_cfg.anonymous_enabled
        );
    }

    if security_mode != SecurityMode::None
        && (opcua_cfg.server_certificate_path.is_none()
            || opcua_cfg.server_private_key_path.is_none())
    {
        return Err(anyhow!(
            "Security mode {:?} requires server_certificate_path and server_private_key_path to be set",
            security_mode
        ));
    }

    let opcua_cfg_internal =
        build_opcua_config(&opcua_cfg, write_mode, security_mode, security_policy);

    opcua_cfg_internal
        .validate()
        .map_err(|e| anyhow!("OPC UA configuration is invalid: {}", e))?;

    let write_handler_arc: Arc<dyn opcua_server::WriteHandler> = Arc::new(write_handler);

    let server = Server::new(
        Arc::new(opcua_cfg_internal),
        registry.clone(),
        write_handler_arc,
    );

    let server_handle: ServerHandle = server
        .start()
        .map_err(|e| anyhow!("OPC UA server error: {}", e))?;

    info!("Runtime started. Press Ctrl-C to shutdown.");

    signal::ctrl_c()
        .await
        .map_err(|e| anyhow!("Failed to listen for ctrl-c: {}", e))?;
    info!("Shutdown signal received, shutting down...");

    for d in &drivers {
        d.stop().await;
    }

    server_handle
        .shutdown()
        .map_err(|e| anyhow!("Failed to request server shutdown: {}", e))?;
    let _ = server_handle.wait().await;

    info!("Shutdown complete.");
    Ok(())
}
