use dioxus::prelude::*;

use crate::models::{Device, TransferStatus};

#[component]
pub fn SendDataModal() -> Element {
    let mut selected_device = use_context::<Signal<Option<Device>>>();
    let mut message = use_signal(String::new);
    let mut transfer_status = use_signal(|| TransferStatus::Idle);

    let device = selected_device();

    if device.is_none() {
        return rsx! {};
    }
    let device = device.unwrap();

    let device_for_send = device.clone();

    rsx! {
        // Modal backdrop
        div {
            class: "modal modal-open modal-bottom sm:modal-middle",

            div {
                class: "modal-box bg-base-100 border border-base-300 shadow-2xl max-w-md",

                // Close button
                button {
                    class: "btn btn-sm btn-circle btn-ghost absolute right-3 top-3",
                    onclick: move |_| {
                        selected_device.set(None);
                        transfer_status.set(TransferStatus::Idle);
                        message.set(String::new());
                    },
                    "✕"
                }

                // Device header
                div {
                    class: "flex items-center gap-4 mb-6",
                    div {
                        class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-primary/10 text-3xl",
                        "{device.device_type.icon()}"
                    }
                    div {
                        h3 {
                            class: "font-bold text-lg",
                            "{device.name}"
                        }
                        p {
                            class: "text-sm text-base-content/50 font-mono",
                            "{device.ip_address}"
                        }
                    }
                }

                // Divider
                div { class: "divider my-0" }

                // Send options
                div {
                    class: "py-4 space-y-4",

                    // Quick actions
                    h4 {
                        class: "text-sm font-semibold text-base-content/60 uppercase tracking-wider",
                        "Quick Actions"
                    }

                    div {
                        class: "grid grid-cols-2 gap-2",
                        // File picker button
                        button {
                            class: "btn btn-outline btn-sm gap-2 rounded-xl",
                            onclick: move |_| {
                                // ┌──────────────────────────────────────────────┐
                                // │ ENGINE HOOK: Open file picker & send file   │
                                // │ Integrate with rfd or native file dialog    │
                                // └──────────────────────────────────────────────┘
                                transfer_status.set(TransferStatus::Error {
                                    message: "File picker not connected yet".into(),
                                });
                            },
                            "📁"
                            "Send File"
                        }
                        // Clipboard button
                        button {
                            class: "btn btn-outline btn-sm gap-2 rounded-xl",
                            onclick: move |_| {
                                // ┌──────────────────────────────────────────────┐
                                // │ ENGINE HOOK: Send clipboard contents        │
                                // │ Read clipboard & send via engine            │
                                // └──────────────────────────────────────────────┘
                                transfer_status.set(TransferStatus::Error {
                                    message: "Clipboard send not connected yet".into(),
                                });
                            },
                            "📋"
                            "Clipboard"
                        }
                    }

                    // Text message input
                    div { class: "divider text-xs text-base-content/40", "or send a message" }

                    div {
                        class: "form-control",
                        textarea {
                            class: "textarea textarea-bordered rounded-xl w-full resize-none h-24 focus:textarea-primary",
                            placeholder: "Type a message to send…",
                            value: "{message}",
                            oninput: move |e| {
                                message.set(e.value());
                            },
                        }
                    }

                    // Send button
                    button {
                        class: "btn btn-primary w-full rounded-xl gap-2 shadow-md",
                        disabled: message().trim().is_empty(),
                        onclick: {
                            let device_id = device_for_send.id.clone();
                            move |_| {
                                // ┌──────────────────────────────────────────────┐
                                // │ ENGINE HOOK: Send text message to device    │
                                // │ Call engine::send_data_to_device(id, msg)   │
                                // └──────────────────────────────────────────────┘
                                let msg = message();
                                let _id = device_id.clone();
                                if !msg.trim().is_empty() {
                                    transfer_status.set(TransferStatus::Sending {
                                        progress: 0.5,
                                        filename: "message".into(),
                                    });
                                    // TODO: Replace with real engine call:
                                    // engine::send_data_to_device(&_id, &msg);
                                    transfer_status.set(TransferStatus::Done {
                                        filename: "message".into(),
                                    });
                                    message.set(String::new());
                                }
                            }
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
                        "Send Message"
                    }

                    // Transfer status
                    match transfer_status() {
                        TransferStatus::Idle => rsx! {},
                        TransferStatus::Sending { progress, filename } => rsx! {
                            div {
                                class: "alert alert-info rounded-xl text-sm py-2",
                                span { class: "loading loading-spinner loading-xs" }
                                span { "Sending {filename}… {progress:.0}%" }
                            }
                        },
                        TransferStatus::Done { filename } => rsx! {
                            div {
                                class: "alert alert-success rounded-xl text-sm py-2",
                                "✅ {filename} sent successfully!"
                            }
                        },
                        TransferStatus::Error { message } => rsx! {
                            div {
                                class: "alert alert-error rounded-xl text-sm py-2",
                                "⚠️ {message}"
                            }
                        },
                    }
                }
            }

            // Backdrop click to close
            div {
                class: "modal-backdrop",
                onclick: move |_| {
                    selected_device.set(None);
                    transfer_status.set(TransferStatus::Idle);
                    message.set(String::new());
                },
            }
        }
    }
}
