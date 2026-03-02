use std::path::PathBuf;

use dioxus::prelude::*;

use crate::engine::bridge::{EngineSender, TransferState};
use crate::models::Device;

#[component]
pub fn SendDataModal() -> Element {
    let mut selected_device = use_context::<Signal<Option<Device>>>();
    let engine_sender = use_context::<Signal<Option<EngineSender>>>();
    let mut transfer_state = use_context::<Signal<TransferState>>();

    let mut chosen_file: Signal<Option<PathBuf>> = use_signal(|| None);
    let mut local_error: Signal<Option<String>> = use_signal(|| None);

    let device = selected_device();

    if device.is_none() {
        return rsx! {};
    }
    let device = device.unwrap();

    let reset_and_close = move |_: Event<MouseData>| {
        selected_device.set(None);
        chosen_file.set(None);
        local_error.set(None);
        transfer_state.set(TransferState::Idle);
    };

    // Whether a transfer is actively running (disable inputs).
    let is_busy = matches!(
        transfer_state(),
        TransferState::Connecting { .. }
            | TransferState::Connected { .. }
            | TransferState::Sending { .. }
    );

    let device_addr = device.socket_addr();
    let device_for_send = device.clone();

    rsx! {
        // Full-screen overlay — manually styled to avoid daisyUI modal
        // classes whose @layer / oklch CSS may not render correctly in
        // the desktop webview, causing a white flash.
        div {
            style: "position:fixed;inset:0;z-index:999;display:flex;align-items:center;justify-content:center;background-color:rgba(0,0,0,0.6);",
            // Clicking the backdrop (outer div) closes the modal
            onclick: reset_and_close,

            // Modal card — stop propagation so clicks inside don't close
            div {
                class: "bg-base-100 border border-base-300 shadow-2xl max-w-md w-full mx-4 rounded-2xl p-6 relative",
                onclick: move |evt: Event<MouseData>| {
                    evt.stop_propagation();
                },

                // Close button
                button {
                    class: "btn btn-sm btn-circle btn-ghost absolute right-3 top-3",
                    onclick: reset_and_close,
                    "✕"
                }

                // Device header
                div {
                    class: "flex items-center gap-4 mb-6",
                    div {
                        class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-primary/10 text-3xl",
                        "{device.icon()}"
                    }
                    div {
                        h3 {
                            class: "font-bold text-lg",
                            "{device.name}"
                        }
                        p {
                            class: "text-sm text-base-content/50 font-mono",
                            "{device_addr}"
                        }
                    }
                }

                div { class: "divider my-0" }

                // Send options
                div {
                    class: "py-4 space-y-4",

                    // Step 1: Pick a file
                    h4 {
                        class: "text-sm font-semibold text-base-content/60 uppercase tracking-wider",
                        "Select a File"
                    }

                    button {
                        class: "btn btn-outline w-full rounded-xl gap-2",
                        disabled: is_busy,
                        onclick: move |_| async move {
                            let file_handle = rfd::AsyncFileDialog::new()
                                .set_title("Select a file to send")
                                .pick_file()
                                .await;

                            if let Some(handle) = file_handle {
                                let path = handle.path().to_path_buf();
                                chosen_file.set(Some(path));
                                local_error.set(None);
                            }
                        },
                        "📁"
                        if chosen_file().is_some() {
                            {
                                chosen_file()
                                    .as_ref()
                                    .and_then(|p| p.file_name())
                                    .map_or_else(
                                        || "Unknown file".to_string(),
                                        |n| n.to_string_lossy().to_string(),
                                    )
                            }
                        } else {
                            "Choose File…"
                        }
                    }

                    // Show file info if selected
                    if let Some(ref path) = chosen_file() {
                        div {
                            class: "text-xs text-base-content/40 font-mono truncate px-1",
                            "{path.display()}"
                        }
                    }

                    // Step 2: Send
                    button {
                        class: "btn btn-primary w-full rounded-xl gap-2 shadow-md",
                        disabled: chosen_file().is_none() || is_busy,
                        onclick: {
                            let addr = device_for_send.socket_addr();
                            move |_| {
                                let addr = addr.clone();
                                async move {
                                    let Some(ref file_path) = chosen_file() else {
                                        return;
                                    };
                                    let Some(ref sender) = engine_sender() else {
                                        local_error.set(Some("Engine is not running".into()));
                                        return;
                                    };

                                    let cmd_tx = sender.cmd_tx.clone();
                                    let path = file_path.clone();

                                    transfer_state.set(TransferState::Connecting {
                                        addr: addr.clone(),
                                    });

                                    if let Err(e) = crate::engine::bridge::send_file(&cmd_tx, &addr, path).await {
                                        tracing::error!(error = %e, "Failed to send file");
                                        transfer_state.set(TransferState::Error {
                                            message: format!("Send failed: {e}"),
                                        });
                                    }
                                }
                            }
                        },
                        if is_busy {
                            span { class: "loading loading-spinner loading-xs" }
                            "Sending…"
                        } else {
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
                            "Send File"
                        }
                    }

                    // Transfer status display
                    match transfer_state() {
                        TransferState::Idle => rsx! {},
                        TransferState::Connecting { addr } => rsx! {
                            div {
                                class: "alert rounded-xl text-sm py-3",
                                span { class: "loading loading-spinner loading-xs" }
                                span { "Connecting to {addr}…" }
                            }
                        },
                        TransferState::Connected { peer_name } => rsx! {
                            div {
                                class: "alert rounded-xl text-sm py-3",
                                span { class: "loading loading-spinner loading-xs" }
                                span { "Connected to {peer_name}, waiting for acceptance…" }
                            }
                        },
                        TransferState::Sending { bytes_sent, total_bytes, .. } => {
                            #[allow(clippy::cast_precision_loss)]
                            let pct = if total_bytes > 0 {
                                (bytes_sent as f64 / total_bytes as f64) * 100.0
                            } else {
                                0.0
                            };
                            rsx! {
                                div {
                                    class: "space-y-2",
                                    div {
                                        class: "flex justify-between text-xs text-base-content/60",
                                        span { "Sending…" }
                                        span { "{pct:.1}%" }
                                    }
                                    progress {
                                        class: "progress progress-primary w-full",
                                        value: "{pct}",
                                        max: "100",
                                    }
                                    div {
                                        class: "text-xs text-base-content/40 text-right font-mono",
                                        "{format_size(bytes_sent)} / {format_size(total_bytes)}"
                                    }
                                }
                            }
                        },
                        TransferState::Done { filename } => rsx! {
                            div {
                                class: "alert alert-success rounded-xl text-sm py-3",
                                "✅ Transfer complete!"
                            }
                            if !filename.is_empty() {
                                div {
                                    class: "text-xs text-base-content/40 mt-1",
                                    "ID: {filename}"
                                }
                            }
                        },
                        TransferState::Error { message } => rsx! {
                            div {
                                class: "alert alert-error rounded-xl text-sm py-3",
                                "⚠️ {message}"
                            }
                        },
                    }

                    // Local errors (e.g. engine not running)
                    if let Some(msg) = local_error() {
                        div {
                            class: "alert alert-warning rounded-xl text-sm py-2",
                            "⚠️ {msg}"
                        }
                    }
                }
            }
        }
    }
}

/// Formats a byte count into a human-readable string.
#[allow(clippy::cast_precision_loss)]
fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
