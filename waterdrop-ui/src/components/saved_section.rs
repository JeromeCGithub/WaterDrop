use dioxus::prelude::*;

use crate::components::device_card::DeviceCard;
use crate::components::empty_state::EmptyState;
use crate::models::Device;

#[component]
pub fn SavedSection() -> Element {
    let saved_devices = use_context::<Signal<Vec<Device>>>();
    let mut show_add_modal = use_context::<Signal<bool>>();

    rsx! {
        section {
            class: "mt-8",

            // Section header
            div {
                class: "flex items-center justify-between mb-4",
                div {
                    class: "flex items-center gap-3",
                    h2 {
                        class: "text-2xl font-bold tracking-tight",
                        "Devices"
                    }
                    span {
                        class: "badge badge-ghost badge-sm",
                        "{saved_devices().len()} saved"
                    }
                }
                button {
                    class: "btn btn-primary btn-sm gap-2 rounded-full shadow-md",
                    onclick: move |_| {
                        show_add_modal.set(true);
                    },
                    svg {
                        class: "w-4 h-4",
                        fill: "none",
                        stroke: "currentColor",
                        "stroke-width": "2",
                        "viewBox": "0 0 24 24",
                        path {
                            d: "M12 4.5v15m7.5-7.5h-15",
                        }
                    }
                    "Add Device"
                }
            }

            // Subtitle
            p {
                class: "text-sm text-base-content/50 mb-4 -mt-2",
                "Your saved devices — tap one to send a file"
            }

            // Device list
            if saved_devices().is_empty() {
                EmptyState {
                    icon: "🖥️",
                    title: "No devices yet".to_string(),
                    subtitle: "Add a device by entering its IP address and port".to_string(),
                }
            } else {
                div {
                    class: "grid gap-3",
                    for device in saved_devices() {
                        DeviceCard {
                            key: "{device.id}",
                            device: device.clone(),
                        }
                    }
                }
            }
        }
    }
}
