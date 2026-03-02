use dioxus::prelude::*;

use crate::components::{CardType, DeviceCard, EmptyState};
use crate::models::Device;

#[component]
pub fn DiscoverSection() -> Element {
    let discovered_devices = use_context::<Signal<Vec<Device>>>();
    let mut is_scanning = use_context::<Signal<bool>>();

    let handle_scan = move |_| {
        // ┌──────────────────────────────────────────────────┐
        // │ ENGINE HOOK: Trigger device discovery scan here  │
        // │ Replace the placeholder with your engine call.   │
        // └──────────────────────────────────────────────────┘
        is_scanning.set(true);
        // Simulate scan completing — replace with real async engine call
        // After scan completes, update discovered_devices signal
        is_scanning.set(false);
    };

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
                        "Discover"
                    }
                    if is_scanning() {
                        span { class: "loading loading-dots loading-sm text-primary" }
                    }
                }
                button {
                    class: "btn btn-primary btn-sm gap-2 rounded-full shadow-md",
                    disabled: is_scanning(),
                    onclick: handle_scan,
                    if is_scanning() {
                        span { class: "loading loading-spinner loading-xs" }
                        "Scanning…"
                    } else {
                        svg {
                            class: "w-4 h-4",
                            fill: "none",
                            stroke: "currentColor",
                            "stroke-width": "2",
                            "viewBox": "0 0 24 24",
                            path {
                                d: "M21 21l-4.35-4.35M18.5 10.5a8 8 0 11-16 0 8 8 0 0116 0z",
                            }
                        }
                        "Scan"
                    }
                }
            }

            // Subtitle
            p {
                class: "text-sm text-base-content/50 mb-4 -mt-2",
                "Devices found on your local network"
            }

            // Device list
            if discovered_devices().is_empty() {
                EmptyState {
                    icon: "📡",
                    title: "No devices found".to_string(),
                    subtitle: "Tap Scan to discover nearby devices".to_string(),
                }
            } else {
                div {
                    class: "grid gap-3",
                    for device in discovered_devices() {
                        DeviceCard {
                            key: "{device.id}",
                            device: device.clone(),
                            card_type: CardType::Discovered,
                        }
                    }
                }
            }
        }
    }
}
