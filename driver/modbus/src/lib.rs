//! Modbus driver crate for Modbus/TCP.
//! Public types are re-exported for convenience.

pub mod config;
pub mod driver;
pub mod errors;
pub mod mapping;
pub mod write_request;

pub use crate::config::ModbusConfig;
pub use crate::driver::ModbusDriver;
pub use crate::errors::DriverError;
pub use crate::mapping::{ModbusFunction, ModbusMapping};
pub use crate::write_request::WriteRequest;

/// Re-export the shared runtime-facing protocol trait so consumers that work with
/// heterogeneous drivers can import it from the driver crate for convenience.
pub use driver_common::ProtocolDriver;
