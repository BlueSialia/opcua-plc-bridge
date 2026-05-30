//! OPC UA server configuration types.
//!
//! This module defines the strongly-typed configuration used by the `opcua-server`
//! crate. It focuses on small, well-documented types and helper methods useful to
//! the server lifecycle (validation, address construction, simple conversions).
//!
//! The public `OpcUaConfig` mirrors the architecture requirements and intentionally
//! contains no runtime side-effects.

use std::net::SocketAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How the OPC UA server should acknowledge write requests to the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Accept write when the request has been queued to the runtime. Fast but
    /// provides no end-to-end confirmation. This should be a deliberate
    /// deployment choice, not an accidental default — use only when write
    /// latency is more critical than end-to-end confirmation.
    QueuedAck,
    /// Wait for a backend/driver confirmation before acknowledging the OPC UA client.
    /// This is the safer default for production.
    #[default]
    ConfirmedAck,
}

/// OPC UA security mode for an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecurityMode {
    /// No security — plaintext OPC UA (UA_TCP). Only suitable for air-gapped
    /// or fully trusted networks.
    #[default]
    None,
    /// Messages are signed but not encrypted.
    Sign,
    /// Messages are signed and encrypted.
    SignAndEncrypt,
}

/// OPC UA security policy URI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecurityPolicy {
    /// No security policy.
    #[serde(rename = "none")]
    #[default]
    None,
    /// Basic128Rsa15
    Basic128Rsa15,
    /// Basic256
    Basic256,
    /// Basic256Sha256
    Basic256Sha256,
}

impl SecurityPolicy {
    /// Return the well-known OPC UA policy URI for this security policy.
    pub fn uri(&self) -> &'static str {
        match self {
            SecurityPolicy::None => "http://opcfoundation.org/UA/SecurityPolicy#None",
            SecurityPolicy::Basic128Rsa15 => {
                "http://opcfoundation.org/UA/SecurityPolicy#Basic128Rsa15"
            }
            SecurityPolicy::Basic256 => "http://opcfoundation.org/UA/SecurityPolicy#Basic256",
            SecurityPolicy::Basic256Sha256 => {
                "http://opcfoundation.org/UA/SecurityPolicy#Basic256Sha256"
            }
        }
    }
}

/// Certificate and trust-store configuration for the OPC UA server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateConfig {
    /// Path to the server's X.509 certificate (DER or PEM).
    pub server_certificate_path: Option<String>,
    /// Path to the server's private key (DER or PEM).
    pub server_private_key_path: Option<String>,
    /// Directory containing trusted client certificates.
    pub trust_store_dir: Option<String>,
    /// Directory for rejected certificates (used for auditing).
    pub reject_store_dir: Option<String>,
    /// Minimum key length required for client certificates (default 2048).
    #[serde(default = "default_min_key_length")]
    pub min_key_length: u32,
}

fn default_min_key_length() -> u32 {
    2048
}

impl Default for CertificateConfig {
    fn default() -> Self {
        Self {
            server_certificate_path: None,
            server_private_key_path: None,
            trust_store_dir: None,
            reject_store_dir: None,
            min_key_length: default_min_key_length(),
        }
    }
}

/// Strongly typed OPC UA adapter configuration.
///
/// This struct is intentionally small and serializable so it can be derived from
/// the runtime configuration (TOML/YAML) the workspace already uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcUaConfig {
    /// Bind address (IP or hostname) for the OPC UA endpoint.
    ///
    /// Example: `0.0.0.0` or `127.0.0.1`
    pub bind_addr: String,

    /// TCP port for the OPC UA endpoint (default 4840).
    pub port: u16,

    /// Human-friendly application name (displayed to clients and in logs).
    pub application_name: String,

    /// Application URI (unique identifier for the server).
    pub application_uri: String,

    /// Namespace URI used when creating nodes in the server namespace.
    pub namespace_uri: String,

    /// Allow anonymous connections. When `false` and `username_password_enabled`
    /// is also `false`, the server will reject all connections unless a certificate
    /// trust path is configured.
    pub anonymous_enabled: bool,

    /// Allow username/password authentication (basic phase-1 support).
    pub username_password_enabled: bool,

    /// Maximum concurrent sessions the server will allow.
    pub max_sessions: u32,

    /// Maximum concurrent subscriptions allowed by the server.
    pub max_subscriptions: u32,

    /// Write acknowledgement mode (queued vs confirmed).
    #[serde(default)]
    pub write_mode: WriteMode,

    /// Security mode for the endpoint (None, Sign, SignAndEncrypt).
    #[serde(default)]
    pub security_mode: SecurityMode,

    /// Security policy for the endpoint.
    #[serde(default)]
    pub security_policy: SecurityPolicy,

    /// Certificate and trust-store configuration.
    #[serde(default)]
    pub certificates: CertificateConfig,
}

impl OpcUaConfig {
    /// Return a reasonable default configuration for local deployment.
    pub fn default_local() -> Self {
        Self {
            bind_addr: "0.0.0.0".to_string(),
            port: 4840,
            application_name: "opcua-plc-bridge".to_string(),
            application_uri: "urn:opcua:plc:bridge".to_string(),
            namespace_uri: "urn:opcua:plc:bridge:ns".to_string(),
            anonymous_enabled: true,
            username_password_enabled: false,
            max_sessions: 100,
            max_subscriptions: 1000,
            write_mode: WriteMode::ConfirmedAck,
            security_mode: SecurityMode::None,
            security_policy: SecurityPolicy::None,
            certificates: CertificateConfig::default(),
        }
    }

    /// Construct the full OPC UA endpoint URL for human consumption.
    ///
    /// Example: `opc.tcp://0.0.0.0:4840`
    pub fn endpoint_url(&self) -> String {
        format!("opc.tcp://{}:{}", self.bind_addr, self.port)
    }

    /// Construct a `SocketAddr` for binding sockets.
    ///
    /// Returns `ConfigError::InvalidAddress` if the composed address does not
    /// parse as a valid socket address.
    pub fn bind_socket_addr(&self) -> Result<SocketAddr, ConfigError> {
        let s = format!("{}:{}", self.bind_addr, self.port);
        SocketAddr::from_str(&s).map_err(|e| ConfigError::InvalidAddress {
            addr: s,
            reason: e.to_string(),
        })
    }

    /// Perform lightweight validation of configuration semantics.
    ///
    /// This checks common misconfigurations such as empty required strings and
    /// port ranges. It returns `Ok(())` for valid configurations or a
    /// `ConfigError` describing the problem.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.application_name.trim().is_empty() {
            return Err(ConfigError::MissingField("application_name".into()));
        }
        if self.application_uri.trim().is_empty() {
            return Err(ConfigError::MissingField("application_uri".into()));
        }
        if self.namespace_uri.trim().is_empty() {
            return Err(ConfigError::MissingField("namespace_uri".into()));
        }
        if self.port == 0 {
            return Err(ConfigError::InvalidPort(self.port));
        }
        // Validate socket address parsing
        let _ = self.bind_socket_addr()?;
        Ok(())
    }
}

/// Errors that can arise when working with `OpcUaConfig`.
#[derive(Error, Debug)]
pub enum ConfigError {
    /// Required string field missing or empty.
    #[error("missing required field: {0}")]
    MissingField(String),

    /// Invalid TCP port provided.
    #[error("invalid port: {0}")]
    InvalidPort(u16),

    /// Address string failed to parse as a `SocketAddr`.
    #[error("invalid bind address `{addr}`: {reason}")]
    InvalidAddress { addr: String, reason: String },

    /// Generic configuration error.
    #[error("configuration error: {0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_local_is_valid() {
        let cfg = OpcUaConfig::default_local();
        cfg.validate().expect("default config must validate");
        assert_eq!(cfg.endpoint_url(), "opc.tcp://0.0.0.0:4840");
    }

    #[test]
    fn invalid_port_rejected() {
        let mut cfg = OpcUaConfig::default_local();
        cfg.port = 0;
        let err = cfg.validate().expect_err("port 0 should be invalid");
        match err {
            ConfigError::InvalidPort(0) => {}
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn empty_strings_rejected() {
        let mut cfg = OpcUaConfig::default_local();
        cfg.application_uri = "".into();
        let err = cfg
            .validate()
            .expect_err("empty application_uri should be invalid");
        match err {
            ConfigError::MissingField(ref s) if s == "application_uri" => {}
            other => panic!("unexpected error: {:?}", other),
        }
    }

    /// Verify that every `SecurityPolicy` variant maps to the correct well-known
    /// OPC UA policy URI as defined by the OPC Foundation.
    /// #feature UA-SEC-POLICIES
    #[test]
    fn security_policy_uris_are_correct() {
        assert_eq!(
            SecurityPolicy::None.uri(),
            "http://opcfoundation.org/UA/SecurityPolicy#None"
        );
        assert_eq!(
            SecurityPolicy::Basic128Rsa15.uri(),
            "http://opcfoundation.org/UA/SecurityPolicy#Basic128Rsa15"
        );
        assert_eq!(
            SecurityPolicy::Basic256.uri(),
            "http://opcfoundation.org/UA/SecurityPolicy#Basic256"
        );
        assert_eq!(
            SecurityPolicy::Basic256Sha256.uri(),
            "http://opcfoundation.org/UA/SecurityPolicy#Basic256Sha256"
        );
    }
}
