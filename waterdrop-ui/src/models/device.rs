use serde::{Deserialize, Serialize};

/// Represents a device that the user has manually added.
/// Devices are persisted to a JSON file and loaded at startup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// Unique identifier (UUID v4, generated when the device is added).
    pub id: String,
    /// Human-readable name chosen by the user (e.g. "Jay's MacBook").
    pub name: String,
    /// IP address or hostname of the device.
    pub address: String,
    /// Port the remote WaterDrop instance listens on.
    pub port: u16,
}

impl Device {
    /// Returns the `address:port` string used to connect via the engine.
    #[must_use]
    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    /// Returns an emoji icon for display purposes.
    #[must_use]
    pub fn icon(&self) -> &'static str {
        "🖥️"
    }
}

/// The list of saved devices, serialized as a JSON array.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceList {
    pub devices: Vec<Device>,
}
