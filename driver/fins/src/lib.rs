//! FINS driver crate for Omron PLCs.
//! Public types are re-exported for convenience.

pub mod config;
pub mod driver;
pub mod errors;
pub mod mapping;
pub mod write_request;

pub use crate::config::FinsConfig;
pub use crate::driver::FinsDriver;
pub use crate::errors::DriverError;
pub use crate::mapping::TagMapping;
pub use crate::write_request::WriteRequest;
