//! Runtime CLI.
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "opcua-plc-bridge-runtime")]
#[command(about = "OPC UA bridge runtime for PLC drivers", long_about = None)]
pub struct Cli {
    /// Runtime configuration file (TOML or YAML).
    #[arg(short, long, value_name = "FILE", default_value = "config/config.toml")]
    pub config: PathBuf,
}
