use dioxus::prelude::*;

use crate::engine::bridge::{EngineSender, IncomingTransfer, ReceiveState};

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

#[component]
pub fn ReceiveFileModal() -> Element {
    let mut receive_state = use_context::<Signal<ReceiveState>>();
    let engine_sender = use_context::<Signal<Option<EngineSender>>>();

    // If idle, render nothing — no modal.
    if matches!(*receive_state.read(), ReceiveState::Idle) {
        return rsx! {};
    }

    // Dismiss: reset to Idle (only allowed when not actively receiving).
    let can_dismiss = matches!(
        *receive_state.read(),
        ReceiveState::Done { .. } | ReceiveState::Error { .. } | ReceiveState::Denied { .. }
    );

    let dismiss = move |_: Event<MouseData>| {
        if can_dismiss {
            receive_state.set(ReceiveState::Idle);
        }
    };

    rsx! {
        // Full-screen overlay — same pattern as the other modals:
        // inline rgba() backdrop to avoid the daisyUI @layer/oklch
        // rendering issues in the desktop webview.
        div {
            style: "position:fixed;inset:0;z-index:999;display:flex;align-items:center;justify-content:center;background-color:rgba(0,0,0,0.6);",
            onclick: move |evt: Event<MouseData>| {
                if can_dismiss {
                    receive_state.set(ReceiveState::Idle);
                }
                // If not dismissable, swallow the click.
                let _ = evt;
            },

            // Modal card
            div {
                class: "bg-base-100 border border-base-300 shadow-2xl max-w-md w-full mx-4 rounded-2xl p-6 relative",
                onclick: move |evt: Event<MouseData>| {
                    evt.stop_propagation();
                },

                // Close button (only when dismissable)
                if can_dismiss {
                    button {
                        class: "btn btn-sm btn-circle btn-ghost absolute right-3 top-3",
                        onclick: dismiss,
                        "✕"
                    }
                }

                // Render content based on current state
                match receive_state() {
                    ReceiveState::Idle => rsx! {},

                    ReceiveState::Offered { incoming } => rsx! {
                        { render_offer(incoming, engine_sender, receive_state) }
                    },

                    ReceiveState::Receiving { filename, bytes_received, total_bytes } => rsx! {
                        { render_receiving(filename, bytes_received, total_bytes) }
                    },

                    ReceiveState::Denied { filename } => rsx! {
                        { render_denied(filename) }
                    },

                    ReceiveState::Done { filename, saved_path } => rsx! {
                        { render_done(filename, saved_path) }
                    },

                    ReceiveState::Error { message } => rsx! {
                        { render_error(message) }
                    },
                }
            }
        }
    }
}

/// Renders the "incoming offer" view with Accept / Deny buttons.
fn render_offer(
    incoming: IncomingTransfer,
    engine_sender: Signal<Option<EngineSender>>,
    mut receive_state: Signal<ReceiveState>,
) -> Element {
    let session_id = incoming.session_id;
    let transfer_id = incoming.transfer_id.clone();
    let filename = incoming.filename.clone();
    let size_bytes = incoming.size_bytes;
    let peer_name = incoming.peer_name.clone();

    let transfer_id_deny = transfer_id.clone();
    let filename_deny = filename.clone();

    rsx! {
        // Header
        div {
            class: "flex items-center gap-4 mb-6",
            div {
                class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-info/10 text-3xl",
                "📥"
            }
            div {
                h3 {
                    class: "font-bold text-lg",
                    "Incoming File"
                }
                p {
                    class: "text-sm text-base-content/50",
                    "From {peer_name}"
                }
            }
        }

        div { class: "divider my-0" }

        // File details
        div {
            class: "py-4 space-y-4",

            // File info card
            div {
                class: "bg-base-200 rounded-xl p-4 space-y-2",
                div {
                    class: "flex items-center gap-3",
                    span { class: "text-2xl", "📄" }
                    div {
                        class: "flex-1 min-w-0",
                        p {
                            class: "font-medium truncate",
                            "{filename}"
                        }
                        p {
                            class: "text-sm text-base-content/50",
                            "{format_size(size_bytes)}"
                        }
                    }
                }
            }

            // Accept / Deny buttons
            div {
                class: "flex gap-3",

                // Deny button
                button {
                    class: "btn btn-outline btn-error flex-1 rounded-xl gap-2",
                    onclick: move |_| {
                        let tid = transfer_id_deny.clone();
                        let fname = filename_deny.clone();
                        async move {
                            if let Some(ref sender) = engine_sender() {
                                if let Err(e) = crate::engine::bridge::respond_to_offer(
                                    &sender.cmd_tx,
                                    session_id,
                                    &tid,
                                    false,
                                ).await {
                                    tracing::error!(error = %e, "Failed to deny transfer");
                                }
                            }
                            receive_state.set(ReceiveState::Denied { filename: fname });
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
                    "Deny"
                }

                // Accept button
                button {
                    class: "btn btn-primary flex-1 rounded-xl gap-2 shadow-md",
                    onclick: move |_| {
                        let tid = transfer_id.clone();
                        async move {
                            if let Some(ref sender) = engine_sender() {
                                if let Err(e) = crate::engine::bridge::respond_to_offer(
                                    &sender.cmd_tx,
                                    session_id,
                                    &tid,
                                    true,
                                ).await {
                                    tracing::error!(error = %e, "Failed to accept transfer");
                                    receive_state.set(ReceiveState::Error {
                                        message: format!("Failed to accept: {e}"),
                                    });
                                }
                            }
                            // The state will transition to Receiving when
                            // progress events arrive from the engine.
                        }
                    },
                    svg {
                        class: "w-4 h-4",
                        fill: "none",
                        stroke: "currentColor",
                        "stroke-width": "2",
                        "viewBox": "0 0 24 24",
                        path {
                            d: "M4.5 12.75l6 6 9-13.5",
                        }
                    }
                    "Accept"
                }
            }
        }
    }
}

/// Renders the "receiving in progress" view with a progress bar.
fn render_receiving(filename: String, bytes_received: u64, total_bytes: u64) -> Element {
    #[allow(clippy::cast_precision_loss)]
    let pct = if total_bytes > 0 {
        (bytes_received as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };

    rsx! {
        // Header
        div {
            class: "flex items-center gap-4 mb-6",
            div {
                class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-info/10 text-3xl",
                "📥"
            }
            div {
                h3 {
                    class: "font-bold text-lg",
                    "Receiving File"
                }
                p {
                    class: "text-sm text-base-content/50",
                    "{filename}"
                }
            }
        }

        div { class: "divider my-0" }

        div {
            class: "py-4 space-y-4",

            // Progress bar section
            div {
                class: "space-y-2",
                div {
                    class: "flex justify-between text-xs text-base-content/60",
                    span { "Receiving…" }
                    span { "{pct:.1}%" }
                }
                progress {
                    class: "progress progress-info w-full",
                    value: "{pct}",
                    max: "100",
                }
                div {
                    class: "text-xs text-base-content/40 text-right font-mono",
                    "{format_size(bytes_received)} / {format_size(total_bytes)}"
                }
            }

            // Informational note
            div {
                class: "flex items-center gap-2 text-sm text-base-content/50",
                span { class: "loading loading-spinner loading-xs" }
                span { "Transfer in progress — please wait…" }
            }
        }
    }
}

/// Renders the "transfer denied" view.
fn render_denied(filename: String) -> Element {
    rsx! {
        // Header
        div {
            class: "flex items-center gap-4 mb-6",
            div {
                class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-warning/10 text-3xl",
                "🚫"
            }
            div {
                h3 {
                    class: "font-bold text-lg",
                    "Transfer Denied"
                }
                p {
                    class: "text-sm text-base-content/50",
                    "You declined the incoming file"
                }
            }
        }

        div { class: "divider my-0" }

        div {
            class: "py-4 space-y-4",
            div {
                class: "alert rounded-xl text-sm py-3",
                "🚫 Denied transfer of \"{filename}\""
            }
        }
    }
}

/// Renders the "transfer complete" view with the file location.
fn render_done(filename: String, saved_path: std::path::PathBuf) -> Element {
    let display_path = saved_path.display().to_string();

    rsx! {
        // Header
        div {
            class: "flex items-center gap-4 mb-6",
            div {
                class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-success/10 text-3xl",
                "✅"
            }
            div {
                h3 {
                    class: "font-bold text-lg",
                    "Transfer Complete"
                }
                p {
                    class: "text-sm text-base-content/50",
                    "File received successfully"
                }
            }
        }

        div { class: "divider my-0" }

        div {
            class: "py-4 space-y-4",

            // Success alert
            div {
                class: "alert alert-success rounded-xl text-sm py-3",
                "🎉 \"{filename}\" received!"
            }

            // File location
            div {
                class: "bg-base-200 rounded-xl p-4 space-y-2",
                p {
                    class: "text-xs font-semibold text-base-content/60 uppercase tracking-wider",
                    "Saved to"
                }
                div {
                    class: "flex items-center gap-3 mt-1",
                    span { class: "text-xl", "📁" }
                    p {
                        class: "text-sm font-mono text-base-content/70 break-all",
                        "{display_path}"
                    }
                }
            }
        }
    }
}

/// Renders the "error" view.
fn render_error(message: String) -> Element {
    rsx! {
        // Header
        div {
            class: "flex items-center gap-4 mb-6",
            div {
                class: "flex items-center justify-center w-14 h-14 rounded-2xl bg-error/10 text-3xl",
                "❌"
            }
            div {
                h3 {
                    class: "font-bold text-lg",
                    "Transfer Failed"
                }
                p {
                    class: "text-sm text-base-content/50",
                    "An error occurred during the transfer"
                }
            }
        }

        div { class: "divider my-0" }

        div {
            class: "py-4 space-y-4",
            div {
                class: "alert alert-error rounded-xl text-sm py-3",
                "⚠️ {message}"
            }
        }
    }
}
