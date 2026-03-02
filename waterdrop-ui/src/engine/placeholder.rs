use crate::models::{Device, DeviceType};

// ╔══════════════════════════════════════════════════════════════════════════════╗
// ║  CONNECT YOUR ENGINE HERE                                                  ║
// ║                                                                            ║
// ║  Replace these placeholder functions with real calls to waterdrop-engine.   ║
// ║                                                                            ║
// ║  1. `fetch_discovered_devices()`  — called by the Discover section.        ║
// ║     Should return `Vec<Device>` from your network scanning logic.          ║
// ║                                                                            ║
// ║  2. `fetch_saved_devices()`       — called by the Saved section.           ║
// ║     Should return `Vec<Device>` from your persistent storage.              ║
// ║                                                                            ║
// ║  3. `save_device(device)`         — called when user clicks "Save" on a    ║
// ║     discovered device. Should persist the device.                          ║
// ║                                                                            ║
// ║  4. `remove_saved_device(id)`     — called when user clicks "Remove" on    ║
// ║     a saved device. Should delete it from storage.                         ║
// ║                                                                            ║
// ║  5. `send_data_to_device(id, payload)` — called when user confirms send.   ║
// ║     Should handle the actual file/data transfer.                           ║
// ╚══════════════════════════════════════════════════════════════════════════════╝

/// Placeholder: replace with engine call to scan for nearby devices.
#[allow(clippy::missing_const_for_fn)]
pub fn fetch_discovered_devices() -> Vec<Device> {
    vec![
        Device {
            id: "disc-1".into(),
            name: "Jay's iPhone".into(),
            ip_address: "192.168.1.42".into(),
            device_type: DeviceType::Phone,
            is_online: true,
        },
        Device {
            id: "disc-2".into(),
            name: "Living Room Mac".into(),
            ip_address: "192.168.1.55".into(),
            device_type: DeviceType::Desktop,
            is_online: true,
        },
        Device {
            id: "disc-3".into(),
            name: "Work Laptop".into(),
            ip_address: "192.168.1.60".into(),
            device_type: DeviceType::Laptop,
            is_online: false,
        },
    ]
}

/// Placeholder: replace with engine call to load saved/bookmarked devices.
#[allow(clippy::missing_const_for_fn)]
pub fn fetch_saved_devices() -> Vec<Device> {
    vec![
        Device {
            id: "saved-1".into(),
            name: "My MacBook Pro".into(),
            ip_address: "192.168.1.10".into(),
            device_type: DeviceType::Laptop,
            is_online: true,
        },
        Device {
            id: "saved-2".into(),
            name: "iPad Air".into(),
            ip_address: "192.168.1.22".into(),
            device_type: DeviceType::Tablet,
            is_online: false,
        },
    ]
}

/// Placeholder: replace with engine call to save/bookmark a device.
#[allow(dead_code)]
pub fn save_device(_device: &Device) {
    // TODO: call waterdrop_engine::save_device(device)
}

/// Placeholder: replace with engine call to remove a saved device.
#[allow(dead_code)]
pub fn remove_saved_device(_device_id: &str) {
    // TODO: call waterdrop_engine::remove_saved_device(device_id)
}

/// Placeholder: replace with engine call to send data to a device.
#[allow(dead_code)]
pub fn send_data_to_device(_device_id: &str, _message: &str) {
    // TODO: call waterdrop_engine::send_data(device_id, payload)
}
