use dioxus::prelude::*;
use tokio::sync::mpsc;

use crate::components::{AddDeviceModal, ReceiveFileModal, SavedSection, SendDataModal, TopBar};
use crate::engine::bridge::{self, BridgeEvent, EngineSender, ReceiveState, TransferState};
use crate::engine::config;
use crate::engine::storage;
use crate::models::Device;

const FAVICON: Asset = asset!("/assets/favicon.ico");
const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");

#[component]
pub fn App() -> Element {
    // ── Global state signals ──────────────────────────────────────────
    let mut engine_sender: Signal<Option<EngineSender>> = use_signal(|| None);
    let mut transfer_state: Signal<TransferState> = use_signal(|| TransferState::Idle);
    let mut receive_state: Signal<ReceiveState> = use_signal(|| ReceiveState::Idle);

    // Load saved devices from disk on startup.
    let saved_devices: Signal<Vec<Device>> = use_signal(storage::load_devices);

    // Whether the "Add Device" modal is open.
    let show_add_modal: Signal<bool> = use_signal(|| false);

    // The device the user clicked on (opens the Send modal).
    let selected_device: Signal<Option<Device>> = use_signal(|| None);

    // Channel for receiving bridge events from the engine event listener
    // (which runs on a tokio::spawn task and therefore cannot touch
    // Dioxus signals directly).
    let mut event_rx: Signal<Option<mpsc::UnboundedReceiver<BridgeEvent>>> = use_signal(|| None);

    // ── Start the engine exactly once ─────────────────────────────────
    use_effect(move || {
        // Only start once — if we already have a sender, skip.
        if engine_sender.peek().is_some() {
            return;
        }

        // Write a default config file if none exists (so users can discover it).
        config::ensure_config_file();

        // Resolve listen address from env vars / config file / default.
        let app_config = config::load();

        match bridge::start_engine(&app_config.listen_addr) {
            Ok(handle) => {
                // Spawn the event listener — it pushes BridgeEvent
                // updates into an mpsc channel (thread-safe).
                let rx = bridge::spawn_event_listener(&handle);

                // Store the sender so components can issue commands.
                engine_sender.set(Some(EngineSender {
                    cmd_tx: handle.cmd_tx.clone(),
                }));

                // Hand the receiver to the use_future below.
                event_rx.set(Some(rx));
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to start engine");
                transfer_state.set(TransferState::Error {
                    message: format!("Engine failed to start: {e}"),
                });
            }
        }
    });

    // ── Pump engine events into Dioxus signals on the main thread ─────
    // use_future runs on the Dioxus async runtime (same thread as
    // signals) so we can safely call .set() here.
    use_future(move || async move {
        loop {
            // Try to receive a bridge event without holding the write
            // lock across the await point (clippy::await_holding_invalid_type).
            let maybe_event = {
                let mut guard = event_rx.write();
                guard.as_mut().and_then(|rx| rx.try_recv().ok())
            };

            if let Some(event) = maybe_event {
                match event {
                    BridgeEvent::Send(state) => {
                        transfer_state.set(state);
                    }
                    BridgeEvent::Receive(state) => {
                        receive_state.set(state);
                    }
                }
                continue;
            }

            // Nothing ready — yield so we don't spin-loop.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });

    // ── Provide global state via context ──────────────────────────────
    use_context_provider(|| saved_devices);
    use_context_provider(|| selected_device);
    use_context_provider(|| show_add_modal);
    use_context_provider(|| engine_sender);
    use_context_provider(|| transfer_state);
    use_context_provider(|| receive_state);

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Stylesheet { href: TAILWIND_CSS }

        div {
            class: "min-h-screen bg-base-200 text-base-content",
            "data-theme": "dark",

            // Top bar
            TopBar {}

            // Main content
            div {
                class: "max-w-5xl mx-auto px-4 sm:px-6 lg:px-8 pb-12",

                // Saved devices section
                SavedSection {}
            }

            // Modals
            AddDeviceModal {}
            SendDataModal {}
            ReceiveFileModal {}
        }
    }
}
