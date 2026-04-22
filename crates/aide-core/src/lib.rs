//! Shared types, paths, and configuration for aide-mcp.

pub mod config;
pub mod paths;

pub use config::{Config, ConfigError, DapConfig, ExecConfig, ScipConfig};
pub use paths::AidePaths;
