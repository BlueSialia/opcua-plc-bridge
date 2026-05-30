//! Runtime configuration types and loader.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Top-level runtime configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub opcua: OpcUaConfig,

    #[serde(default)]
    pub plcs: Vec<PlcConfig>,

    /// Capacity for the internal health mpsc channel used to transmit PLC health snapshots.
    #[serde(default)]
    pub health_channel_capacity: Option<usize>,

    /// Timeout in milliseconds used when enqueuing driver write requests.
    #[serde(default)]
    pub driver_write_send_timeout_ms: Option<u64>,

    /// Timeout in milliseconds to wait for driver write confirmations.
    #[serde(default)]
    pub write_confirm_timeout_ms: Option<u64>,
}

/// OPC UA configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcUaConfig {
    pub bind_addr: String,
    pub port: u16,
    pub application_name: String,
    pub application_uri: String,
    pub namespace_uri: String,

    #[serde(default = "default_true")]
    pub anonymous_enabled: bool,

    #[serde(default)]
    pub username_password_enabled: bool,

    #[serde(default = "default_u32_100")]
    pub max_sessions: u32,

    #[serde(default = "default_u32_1000")]
    pub max_subscriptions: u32,

    /// Write acknowledgement mode: "QueuedAck" or "ConfirmedAck".
    /// Must be one of the two recognized strings; unknown values cause
    /// a startup failure rather than a silent fallback to defaults.
    #[serde(default)]
    pub write_mode: String,

    /// Security mode: "None", "Sign", or "SignAndEncrypt".
    /// Must be one of the recognized strings; unknown values fail startup.
    #[serde(default)]
    pub security_mode: Option<String>,

    /// Security policy: "None", "Basic128Rsa15", "Basic256", or "Basic256Sha256".
    /// Must be one of the recognized strings; unknown values fail startup.
    #[serde(default)]
    pub security_policy: Option<String>,

    /// Path to the server's X.509 certificate (DER or PEM).
    #[serde(default)]
    pub server_certificate_path: Option<String>,

    /// Path to the server's private key (DER or PEM).
    #[serde(default)]
    pub server_private_key_path: Option<String>,

    /// Directory containing trusted client certificates.
    #[serde(default)]
    pub trust_store_dir: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_u32_100() -> u32 {
    100
}
fn default_u32_1000() -> u32 {
    1000
}

/// PLC configuration block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlcConfig {
    /// Logical name for the PLC (used in logs, health events, and OPC UA browse folders).
    pub name: String,

    /// Protocol identifier: `"fins"` or `"modbus"`.
    pub protocol: String,

    /// Host:port endpoint for the PLC (e.g. `"192.168.1.10:9600"`).
    pub endpoint: String,

    /// Modbus unit/slave id (1-255). Required for Modbus protocol, ignored for FINS.
    /// When absent for Modbus, defaults to 1.
    #[serde(default)]
    pub unit_id: Option<u8>,

    #[serde(default = "default_cycle_ms")]
    pub cycle_ms: u64,

    #[serde(default)]
    pub tags: Vec<TagConfig>,

    #[serde(default)]
    pub max_words_per_request: Option<u32>,
}

fn default_cycle_ms() -> u64 {
    100
}

/// Per-tag configuration as expressed in the runtime config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagConfig {
    pub id: String,
    pub name: String,
    pub address: String,
    pub data_type: String,

    #[serde(default)]
    pub writable: bool,

    #[serde(default)]
    pub byte_order: Option<String>,

    #[serde(default)]
    pub word_count: Option<u16>,

    /// Optional FINS memory area code (e.g. 0x82 for D area). When provided,
    /// the address is interpreted as a register offset within this area.
    /// If absent, the area is inferred from the address string convention.
    #[serde(default)]
    pub area: Option<u8>,
}

/// Validation error returned when config values are semantically invalid.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ConfigValidationError(pub String);

/// Parse write_mode string into the internal enum, failing on unknown values.
pub fn parse_write_mode(s: &str) -> Result<opcua_server::WriteMode, ConfigValidationError> {
    match s {
        "QueuedAck" => Ok(opcua_server::WriteMode::QueuedAck),
        "ConfirmedAck" => Ok(opcua_server::WriteMode::ConfirmedAck),
        "" => Ok(opcua_server::WriteMode::default()),
        other => Err(ConfigValidationError(format!(
            "Invalid write_mode '{}'; must be QueuedAck or ConfirmedAck",
            other
        ))),
    }
}

/// Parse security_mode string into the internal enum, failing on unknown values.
pub fn parse_security_mode(
    s: Option<&str>,
) -> Result<opcua_server::SecurityMode, ConfigValidationError> {
    match s {
        None | Some("") | Some("None") => Ok(opcua_server::SecurityMode::None),
        Some("Sign") => Ok(opcua_server::SecurityMode::Sign),
        Some("SignAndEncrypt") => Ok(opcua_server::SecurityMode::SignAndEncrypt),
        Some(other) => Err(ConfigValidationError(format!(
            "Invalid security_mode '{}'; must be None, Sign, or SignAndEncrypt",
            other
        ))),
    }
}

/// Parse security_policy string into the internal enum, failing on unknown values.
pub fn parse_security_policy(
    s: Option<&str>,
) -> Result<opcua_server::SecurityPolicy, ConfigValidationError> {
    match s {
        None | Some("") | Some("None") => Ok(opcua_server::SecurityPolicy::None),
        Some("Basic128Rsa15") => Ok(opcua_server::SecurityPolicy::Basic128Rsa15),
        Some("Basic256") => Ok(opcua_server::SecurityPolicy::Basic256),
        Some("Basic256Sha256") => Ok(opcua_server::SecurityPolicy::Basic256Sha256),
        Some(other) => Err(ConfigValidationError(format!(
            "Invalid security_policy '{}'; must be None, Basic128Rsa15, Basic256, or Basic256Sha256",
            other
        ))),
    }
}

/// Load a runtime configuration from path + text.
pub fn load_runtime_config(path: &Path, text: &str) -> Result<RuntimeConfig, String> {
    let cfg: RuntimeConfig = match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
    {
        Some(ext) if ext == "toml" => {
            toml::from_str::<RuntimeConfig>(text).map_err(|e| format!("TOML parse error: {}", e))?
        }
        Some(ext) if ext == "yaml" || ext == "yml" => {
            serde_yaml::from_str::<RuntimeConfig>(text)
                .map_err(|e| format!("YAML parse error: {}", e))?
        }
        _ => match toml::from_str::<RuntimeConfig>(text) {
            Ok(cfg) => cfg,
            Err(toml_err) => match serde_yaml::from_str::<RuntimeConfig>(text) {
                Ok(cfg) => cfg,
                Err(yaml_err) => {
                    return Err(format!(
                        "Config parse failed: TOML error: {}; YAML error: {}",
                        toml_err, yaml_err
                    ))
                }
            },
        },
    };

    for plc in &cfg.plcs {
        if plc.cycle_ms < 10 {
            return Err(format!(
                "Invalid cycle_ms for {}: {} (must be >= 10)",
                plc.name, plc.cycle_ms
            ));
        }
    }

    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_mode_strict_parsing() {
        assert!(parse_write_mode("QueuedAck").is_ok());
        assert!(parse_write_mode("ConfirmedAck").is_ok());
        assert!(parse_write_mode("").is_ok());
        assert!(parse_write_mode("BadValue").is_err());
    }

    #[test]
    fn security_mode_strict_parsing() {
        assert!(parse_security_mode(Some("None")).is_ok());
        assert!(parse_security_mode(Some("Sign")).is_ok());
        assert!(parse_security_mode(Some("SignAndEncrypt")).is_ok());
        assert!(parse_security_mode(None).is_ok());
        assert!(parse_security_mode(Some("InsecureStuff")).is_err());
    }

    #[test]
    fn security_policy_strict_parsing() {
        assert!(parse_security_policy(Some("None")).is_ok());
        assert!(parse_security_policy(Some("Basic256Sha256")).is_ok());
        assert!(parse_security_policy(None).is_ok());
        assert!(parse_security_policy(Some("Unknown")).is_err());
    }
}
