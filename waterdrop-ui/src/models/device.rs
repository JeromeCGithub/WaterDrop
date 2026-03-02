/// Represents a device discovered on the network or saved by the user.
/// This struct is the bridge between the UI and the engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub ip_address: String,
    pub device_type: DeviceType,
    pub is_online: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum DeviceType {
    Phone,
    Laptop,
    Desktop,
    Tablet,
    Unknown,
}

impl DeviceType {
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Phone => "📱",
            Self::Laptop => "💻",
            Self::Desktop => "🖥️",
            Self::Tablet => "📲",
            Self::Unknown => "❓",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Phone => "Phone",
            Self::Laptop => "Laptop",
            Self::Desktop => "Desktop",
            Self::Tablet => "Tablet",
            Self::Unknown => "Unknown",
        }
    }
}
