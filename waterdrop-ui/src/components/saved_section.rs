use dioxus::prelude::*;

use crate::components::device_card::{CardType, DeviceCard};
use crate::components::empty_state::EmptyState;
use crate::models::Device;

#[component]
pub fn SavedSection() -> Element {
    let saved_devices = use_context::<Signal<Vec<Device>>>();

    rsx! {
        section {
            class: "mt-4",

            // Section header
            div {
                class: "flex items-center justify-between mb-4",
                h2 {
                    class: "text-2xl font-bold tracking-tight",
                    "Saved Devices"
                }
                span {
                    class: "badge badge-ghost badge-sm",
                    "{saved_devices().len()} saved"
                }
            }

            // Subtitle
            p {
                class: "text-sm text-base-content/50 mb-4 -mt-2",
                "Your bookmarked devices for quick access"
            }

            // Device list
            if saved_devices().is_empty() {
                EmptyState {
                    icon: "⭐",
                    title: "No saved devices".to_string(),
                    subtitle: "Save a discovered device for quick access".to_string(),
                }
            } else {
                div {
                    class: "grid gap-3",
                    for device in saved_devices() {
                        DeviceCard {
                            key: "{device.id}",
                            device: device.clone(),
                            card_type: CardType::Saved,
                        }
                    }
                }
            }
        }
    }
}
