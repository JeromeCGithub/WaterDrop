use dioxus::prelude::*;

use crate::models::Device;

#[derive(Debug, Clone, PartialEq)]
pub enum CardType {
    Discovered,
    Saved,
}

#[component]
pub fn DeviceCard(device: Device, card_type: CardType) -> Element {
    let mut selected_device = use_context::<Signal<Option<Device>>>();
    let mut saved_devices = use_context::<Signal<Vec<Device>>>();

    let status_class = if device.is_online {
        "badge-success"
    } else {
        "badge-ghost"
    };
    let status_text = if device.is_online {
        "Online"
    } else {
        "Offline"
    };

    let device_for_select = device.clone();
    let device_for_action = device.clone();

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
                    "{device.device_type.icon()}"
                }

                // Info
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
                            "{device.ip_address}"
                        }
                        span {
                            class: "text-xs text-base-content/40",
                            "·"
                        }
                        span {
                            class: "text-xs text-base-content/50",
                            "{device.device_type.label()}"
                        }
                    }
                }

                // Status badge + action button
                div {
                    class: "flex items-center gap-2 shrink-0",
                    span {
                        class: "badge badge-xs {status_class} gap-1",
                        span { class: "w-1.5 h-1.5 rounded-full bg-current" }
                        "{status_text}"
                    }

                    match card_type {
                        CardType::Discovered => rsx! {
                            button {
                                class: "btn btn-ghost btn-xs btn-circle tooltip tooltip-left",
                                "data-tip": "Save device",
                                onclick: move |evt: Event<MouseData>| {
                                    evt.stop_propagation();
                                    // ┌──────────────────────────────────────────────┐
                                    // │ ENGINE HOOK: Save device here                │
                                    // │ Call engine::save_device(&device)            │
                                    // └──────────────────────────────────────────────┘
                                    let dev = device_for_action.clone();
                                    saved_devices.write().push(dev);
                                },
                                "⭐"
                            }
                        },
                        CardType::Saved => rsx! {
                            button {
                                class: "btn btn-ghost btn-xs btn-circle tooltip tooltip-left",
                                "data-tip": "Remove device",
                                onclick: move |evt: Event<MouseData>| {
                                    evt.stop_propagation();
                                    // ┌──────────────────────────────────────────────┐
                                    // │ ENGINE HOOK: Remove saved device here        │
                                    // │ Call engine::remove_saved_device(&id)        │
                                    // └──────────────────────────────────────────────┘
                                    let id = device_for_action.id.clone();
                                    saved_devices.write().retain(|d| d.id != id);
                                },
                                "✕"
                            }
                        },
                    }
                }
            }
        }
    }
}
