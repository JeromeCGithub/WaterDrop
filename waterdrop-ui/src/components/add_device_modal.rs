use dioxus::prelude::*;

use crate::engine::storage;
use crate::models::Device;

#[component]
pub fn AddDeviceModal() -> Element {
    let mut show_add_modal = use_context::<Signal<bool>>();
    let mut saved_devices = use_context::<Signal<Vec<Device>>>();

    let mut name = use_signal(String::new);
    let mut address = use_signal(String::new);
    let mut port = use_signal(|| "4242".to_string());
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);

    if !show_add_modal() {
        return rsx! {};
    }

    let close = move |_| {
        show_add_modal.set(false);
        name.set(String::new());
        address.set(String::new());
        port.set("4242".to_string());
        error_msg.set(None);
    };

    let can_submit = !name().trim().is_empty()
        && !address().trim().is_empty()
        && !port().trim().is_empty()
        && port().trim().parse::<u16>().is_ok();

    rsx! {
        // Full-screen overlay — manually styled to avoid daisyUI modal
        // classes whose @layer / oklch CSS may not render correctly in
        // the desktop webview, causing a white flash.
        div {
            style: "position:fixed;inset:0;z-index:999;display:flex;align-items:center;justify-content:center;background-color:rgba(0,0,0,0.6);",
            // Clicking the backdrop (outer div) closes the modal
            onclick: close,

            // Modal card — stop propagation so clicks inside don't close
            div {
                class: "bg-base-100 border border-base-300 shadow-2xl max-w-md w-full mx-4 rounded-2xl p-6 relative",
                onclick: move |evt: Event<MouseData>| {
                    evt.stop_propagation();
                },

                // Close button
                button {
                    class: "btn btn-sm btn-circle btn-ghost absolute right-3 top-3",
                    onclick: close,
                    "✕"
                }

                // Header
                div {
                    class: "flex items-center gap-4 mb-6",
                    div {
                        class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-primary/10 text-3xl",
                        "➕"
                    }
                    div {
                        h3 {
                            class: "font-bold text-lg",
                            "Add Device"
                        }
                        p {
                            class: "text-sm text-base-content/50",
                            "Enter the device's connection details"
                        }
                    }
                }

                div { class: "divider my-0" }

                // Form
                div {
                    class: "py-4 space-y-4",

                    // Name field
                    div {
                        class: "form-control",
                        label {
                            class: "label",
                            span { class: "label-text font-medium", "Device Name" }
                        }
                        input {
                            class: "input input-bordered rounded-xl w-full focus:input-primary",
                            r#type: "text",
                            placeholder: "e.g. Jay's MacBook",
                            value: "{name}",
                            oninput: move |e| {
                                name.set(e.value());
                            },
                        }
                    }

                    // Address field
                    div {
                        class: "form-control",
                        label {
                            class: "label",
                            span { class: "label-text font-medium", "IP Address" }
                        }
                        input {
                            class: "input input-bordered rounded-xl w-full focus:input-primary font-mono",
                            r#type: "text",
                            placeholder: "e.g. 192.168.1.42",
                            value: "{address}",
                            oninput: move |e| {
                                address.set(e.value());
                            },
                        }
                    }

                    // Port field
                    div {
                        class: "form-control",
                        label {
                            class: "label",
                            span { class: "label-text font-medium", "Port" }
                        }
                        input {
                            class: "input input-bordered rounded-xl w-full focus:input-primary font-mono",
                            r#type: "text",
                            placeholder: "4242",
                            value: "{port}",
                            oninput: move |e| {
                                port.set(e.value());
                            },
                        }
                    }

                    // Error message
                    if let Some(msg) = error_msg() {
                        div {
                            class: "alert alert-error rounded-xl text-sm py-2",
                            "⚠️ {msg}"
                        }
                    }

                    // Submit button
                    button {
                        class: "btn btn-primary w-full rounded-xl gap-2 shadow-md mt-2",
                        disabled: !can_submit,
                        onclick: move |_| {
                            let trimmed_name = name().trim().to_string();
                            let trimmed_addr = address().trim().to_string();
                            let trimmed_port = port().trim().to_string();

                            let parsed_port = match trimmed_port.parse::<u16>() {
                                Ok(p) => p,
                                Err(_) => {
                                    error_msg.set(Some("Port must be a number between 0 and 65535".into()));
                                    return;
                                }
                            };

                            if trimmed_name.is_empty() || trimmed_addr.is_empty() {
                                error_msg.set(Some("Name and address are required".into()));
                                return;
                            }

                            let new_device = Device {
                                id: uuid::Uuid::new_v4().to_string(),
                                name: trimmed_name,
                                address: trimmed_addr,
                                port: parsed_port,
                            };

                            // Add to the signal and persist.
                            {
                                let mut devices = saved_devices.write();
                                devices.push(new_device);
                                if let Err(e) = storage::save_devices(&devices) {
                                    tracing::error!(error = %e, "Failed to persist new device");
                                    error_msg.set(Some(format!("Failed to save: {e}")));
                                    return;
                                }
                            }

                            // Reset form and close modal.
                            name.set(String::new());
                            address.set(String::new());
                            port.set("4242".to_string());
                            error_msg.set(None);
                            show_add_modal.set(false);
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
            }
        }
    }
}
