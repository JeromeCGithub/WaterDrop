use dioxus::prelude::*;

use crate::engine::storage;
use crate::models::Device;

#[component]
pub fn DeviceCard(device: Device) -> Element {
    let mut selected_device = use_context::<Signal<Option<Device>>>();
    let mut saved_devices = use_context::<Signal<Vec<Device>>>();

    let device_for_select = device.clone();
    let device_for_remove = device.clone();

    rsx! {
        div {
            class: "card bg-base-100 shadow-md hover:shadow-lg transition-all duration-200 border border-base-300/50 hover:border-primary/30 cursor-pointer active:scale-[0.98]",
            onclick: move |_| {
                selected_device.set(Some(device_for_select.clone()));
            },

            div {
                class: "card-body p-4 flex-row items-center gap-4",

                // Device icon
                div {
                    class: "flex items-center justify-center w-12 h-12 rounded-2xl bg-primary/10 text-2xl shrink-0",
                    "{device.icon()}"
                }

                // Device info
                div {
                    class: "flex-1 min-w-0",
                    h3 {
                        class: "font-semibold text-base truncate",
                        "{device.name}"
                    }
                    div {
                        class: "flex items-center gap-2 mt-1",
                        span {
                            class: "text-xs text-base-content/50 font-mono",
                            "{device.address}"
                        }
                        span {
                            class: "text-xs text-base-content/40",
                            ":"
                        }
                        span {
                            class: "text-xs text-base-content/50 font-mono",
                            "{device.port}"
                        }
                    }
                }

                // Action buttons
                div {
                    class: "flex items-center gap-1 shrink-0",

                    // Send file button
                    button {
                        class: "btn btn-ghost btn-sm btn-circle tooltip tooltip-left",
                        "data-tip": "Send file",
                        onclick: move |evt: Event<MouseData>| {
                            evt.stop_propagation();
                            selected_device.set(Some(device.clone()));
                        },
                        svg {
                            class: "w-4 h-4",
                            fill: "none",
                            stroke: "currentColor",
                            "stroke-width": "2",
                            "viewBox": "0 0 24 24",
                            path {
                                d: "M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5",
                            }
                        }
                    }

                    // Remove button
                    button {
                        class: "btn btn-ghost btn-sm btn-circle tooltip tooltip-left text-error/60 hover:text-error",
                        "data-tip": "Remove device",
                        onclick: move |evt: Event<MouseData>| {
                            evt.stop_propagation();
                            let id = device_for_remove.id.clone();
                            let mut devices = saved_devices.write();
                            devices.retain(|d| d.id != id);
                            // Persist the change to disk.
                            if let Err(e) = storage::save_devices(&devices) {
                                tracing::error!(error = %e, "Failed to save devices after removal");
                            }
                        },
                        svg {
                            class: "w-4 h-4",
                            fill: "none",
                            stroke: "currentColor",
                            "stroke-width": "2",
                            "viewBox": "0 0 24 24",
                            path {
                                d: "M6 18L18 6M6 6l12 12",
                            }
                        }
                    }
                }
            }
        }
    }
}
