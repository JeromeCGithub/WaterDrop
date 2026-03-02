//! Application configuration.
//!
//! The listen address for the engine can be configured via:
//!
//! 1. **Environment variable** `WATERDROP_PORT` — just the port number
//!    (e.g. `4242`). The bind address will be `0.0.0.0:<port>`.
//! 2. **Environment variable** `WATERDROP_LISTEN_ADDR` — full address
//!    (e.g. `0.0.0.0:5000`). Takes precedence over `WATERDROP_PORT`.
//! 3. **Config file** `~/.local/share/waterdrop/config.json` (Linux) or
//!    the platform equivalent. The file is a JSON object with optional
//!    fields `listen_addr` and/or `port`.
//! 4. **Default** — `0.0.0.0:4242`.
//!
//! Resolution order (highest priority first):
//!
//! `WATERDROP_LISTEN_ADDR` > `WATERDROP_PORT` > config file `listen_addr`
//! > config file `port` > compiled-in default.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::storage;

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 4242;

/// On-disk representation of the config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    /// Full listen address (e.g. `"0.0.0.0:5000"`).
    /// If set, `port` is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_addr: Option<String>,

    /// Port number only. Combined with `0.0.0.0` as the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// Resolved application configuration ready for use.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// The address the engine should bind to (e.g. `0.0.0.0:4242`).
    pub listen_addr: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen_addr: format!("{DEFAULT_HOST}:{DEFAULT_PORT}"),
        }
    }
}

/// Returns the path to the configuration file.
fn config_file_path() -> PathBuf {
    storage::data_dir_path().join("config.json")
}

/// Loads the config file from disk, returning `None` if it doesn't
/// exist or cannot be parsed.
fn load_config_file() -> Option<ConfigFile> {
    let path = config_file_path();
    debug!(path = %path.display(), "Looking for config file");

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(error = %e, path = %path.display(), "Failed to read config file");
            }
            return None;
        }
    };

    match serde_json::from_str::<ConfigFile>(&content) {
        Ok(cfg) => {
            debug!(?cfg, "Loaded config file");
            Some(cfg)
        }
        Err(e) => {
            warn!(error = %e, path = %path.display(), "Failed to parse config file");
            None
        }
    }
}

/// Saves a default config file to disk if one does not already exist.
///
/// This is a convenience so users can discover the file and edit it.
pub fn ensure_config_file() {
    let path = config_file_path();
    if path.exists() {
        return;
    }

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let default = ConfigFile {
        listen_addr: None,
        port: Some(DEFAULT_PORT),
    };

    match serde_json::to_string_pretty(&default) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(error = %e, path = %path.display(), "Failed to write default config file");
            } else {
                debug!(path = %path.display(), "Created default config file");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to serialize default config");
        }
    }
}

/// Resolves the application configuration from all sources.
///
/// See module-level documentation for the resolution order.
pub fn load() -> AppConfig {
    // 1. Check WATERDROP_LISTEN_ADDR env var (full address).
    if let Ok(addr) = std::env::var("WATERDROP_LISTEN_ADDR") {
        let addr = addr.trim().to_string();
        if !addr.is_empty() {
            info!(listen_addr = %addr, source = "env:WATERDROP_LISTEN_ADDR", "Resolved listen address");
            return AppConfig { listen_addr: addr };
        }
    }

    // 2. Check WATERDROP_PORT env var (port only).
    if let Ok(port_str) = std::env::var("WATERDROP_PORT") {
        let port_str = port_str.trim().to_string();
        if let Ok(port) = port_str.parse::<u16>() {
            let addr = format!("{DEFAULT_HOST}:{port}");
            info!(listen_addr = %addr, source = "env:WATERDROP_PORT", "Resolved listen address");
            return AppConfig { listen_addr: addr };
        } else if !port_str.is_empty() {
            warn!(
                value = %port_str,
                "WATERDROP_PORT is not a valid port number, falling through"
            );
        }
    }

    // 3. Check config file.
    if let Some(cfg) = load_config_file() {
        if let Some(addr) = cfg.listen_addr {
            let addr = addr.trim().to_string();
            if !addr.is_empty() {
                info!(listen_addr = %addr, source = "config_file:listen_addr", "Resolved listen address");
                return AppConfig { listen_addr: addr };
            }
        }
        if let Some(port) = cfg.port {
            let addr = format!("{DEFAULT_HOST}:{port}");
            info!(listen_addr = %addr, source = "config_file:port", "Resolved listen address");
            return AppConfig { listen_addr: addr };
        }
    }

    // 4. Default.
    let config = AppConfig::default();
    info!(listen_addr = %config.listen_addr, source = "default", "Resolved listen address");
    config
}
