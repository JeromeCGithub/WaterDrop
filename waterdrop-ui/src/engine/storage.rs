use std::path::PathBuf;

use tracing::{debug, warn};

use crate::models::{Device, DeviceList};

/// Returns the path to the WaterDrop data directory.
///
/// On Linux: `~/.local/share/waterdrop`
/// On macOS: `~/Library/Application Support/waterdrop`
/// On Windows: `%APPDATA%/waterdrop`
pub(crate) fn data_dir_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("waterdrop")
}

/// Returns the path to the saved devices JSON file.
pub fn devices_file_path() -> PathBuf {
    data_dir_path().join("devices.json")
}

/// Returns the path to the certificate directory used by the QUIC transport.
pub fn cert_dir() -> PathBuf {
    data_dir_path().join("cert")
}

/// Returns the path to the trust store directory used by TOFU verification.
pub fn trust_store_dir() -> PathBuf {
    data_dir_path().join("trusted")
}

/// Returns the default directory where received files are stored.
pub fn receive_dir() -> PathBuf {
    data_dir_path().join("received")
}

/// Loads the list of saved devices from disk.
///
/// Returns an empty list if the file does not exist or cannot be parsed.
pub fn load_devices() -> Vec<Device> {
    let path = devices_file_path();
    debug!(path = %path.display(), "Loading saved devices");

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(error = %e, path = %path.display(), "Failed to read devices file");
            }
            return Vec::new();
        }
    };

    match serde_json::from_str::<DeviceList>(&content) {
        Ok(list) => {
            debug!(count = list.devices.len(), "Loaded saved devices");
            list.devices
        }
        Err(e) => {
            warn!(error = %e, path = %path.display(), "Failed to parse devices file");
            Vec::new()
        }
    }
}

/// Persists the list of saved devices to disk.
///
/// Creates parent directories if they don't exist.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
pub fn save_devices(devices: &[Device]) -> anyhow::Result<()> {
    let path = devices_file_path();
    debug!(path = %path.display(), count = devices.len(), "Saving devices");

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let list = DeviceList {
        devices: devices.to_vec(),
    };

    let json = serde_json::to_string_pretty(&list)?;
    std::fs::write(&path, json)?;

    debug!("Devices saved successfully");
    Ok(())
}

/// Ensures all required data directories exist.
///
/// Creates `cert/`, `trusted/`, and `received/` inside the data dir.
pub fn ensure_dirs() {
    for dir in [
        data_dir_path(),
        cert_dir(),
        trust_store_dir(),
        receive_dir(),
    ] {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(
                error = %e,
                path = %dir.display(),
                "Failed to create data directory"
            );
        }
    }
}
