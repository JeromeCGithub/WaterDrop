use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use waterdrop_engine::engine::{Engine, EngineCmd, EngineConfig, EngineEvent, EngineHandle};
use waterdrop_engine::quic::{QuicConnector, QuicListenerFactory};
use waterdrop_engine::session::{SendRequest, SessionEvent};

use super::storage;

/// Thin wrapper so we can share the command sender across components via context.
///
/// `EngineHandle` itself is not `Clone`, so we extract just the
/// `mpsc::Sender<EngineCmd>` which *is* `Clone`.
#[derive(Clone)]
pub struct EngineSender {
    pub cmd_tx: mpsc::Sender<EngineCmd>,
}

// ─── Outgoing (send) state ────────────────────────────────────────────

/// The status of an outgoing file transfer that the UI can display.
#[derive(Debug, Clone, PartialEq)]
pub enum TransferState {
    /// No transfer is in progress.
    Idle,
    /// Connecting to the remote peer.
    Connecting { addr: String },
    /// The handshake succeeded and we know the peer's name.
    Connected { peer_name: String },
    /// The remote peer accepted our offer — data is flowing.
    Sending {
        filename: String,
        bytes_sent: u64,
        total_bytes: u64,
    },
    /// The transfer finished successfully.
    Done { filename: String },
    /// Something went wrong.
    Error { message: String },
}

// ─── Incoming (receive) state ─────────────────────────────────────────

/// Represents a pending incoming transfer offer from a remote peer.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingTransfer {
    /// Engine session ID — needed to send accept/deny commands.
    pub session_id: u64,
    /// Unique transfer identifier from the protocol.
    pub transfer_id: String,
    /// The filename the sender wants to deliver.
    pub filename: String,
    /// Size of the file in bytes.
    pub size_bytes: u64,
    /// Name of the remote peer device (from the HELLO handshake).
    pub peer_name: String,
}

/// The status of an incoming file receive that the UI can display.
#[derive(Debug, Clone, PartialEq)]
pub enum ReceiveState {
    /// No incoming transfer is happening.
    Idle,
    /// An offer has arrived and is waiting for user decision.
    Offered { incoming: IncomingTransfer },
    /// The user accepted and data is being received.
    Receiving {
        filename: String,
        bytes_received: u64,
        total_bytes: u64,
    },
    /// The transfer was denied by the user.
    Denied { filename: String },
    /// The transfer completed successfully.
    Done {
        filename: String,
        saved_path: PathBuf,
    },
    /// Something went wrong during the receive.
    Error { message: String },
}

/// Unified event type forwarded from the background event-listener
/// task to the Dioxus main thread via an mpsc channel.
///
/// We use a single channel with two variants so the UI pump loop
/// stays simple.
#[derive(Debug, Clone)]
pub enum BridgeEvent {
    /// An update to the *outgoing* transfer state.
    Send(TransferState),
    /// An update to the *incoming* receive state.
    Receive(ReceiveState),
}

// ─── Helpers ──────────────────────────────────────────────────────────

/// Generates a unique transfer ID from the current timestamp.
fn generate_transfer_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("xfer-{ts}")
}

// ─── Engine lifecycle ─────────────────────────────────────────────────

/// Starts the WaterDrop engine on the Tokio runtime.
///
/// - Binds a QUIC listener on `listen_addr` so we can also *receive*
///   files.
/// - Returns an [`EngineHandle`] whose `cmd_tx` / `events_tx` let us
///   drive and observe the engine.
///
/// # Errors
///
/// Returns an error if the TLS certificate directory cannot be
/// initialised.
pub fn start_engine(listen_addr: &str) -> anyhow::Result<EngineHandle> {
    storage::ensure_dirs();

    let cert_dir = storage::cert_dir();
    let trust_store = storage::trust_store_dir();
    let receive_dir = storage::receive_dir();

    let config = EngineConfig {
        device_name: "WaterDrop-UI".into(),
        receive_dir,
    };

    let factory = QuicListenerFactory::new(&cert_dir)?;
    let connector = QuicConnector::new(&trust_store);

    let engine = Engine;
    let handle = engine.start(factory, connector, config);

    // Tell the engine to start accepting inbound connections.
    let cmd_tx = handle.cmd_tx.clone();
    let addr = listen_addr.to_string();
    tokio::spawn(async move {
        if let Err(e) = cmd_tx.send(EngineCmd::StartAccepting { addr }).await {
            warn!(error = %e, "Failed to send StartAccepting command");
        }
    });

    info!("Engine started");
    Ok(handle)
}

// ─── Outgoing: send file ──────────────────────────────────────────────

/// Sends a file to a remote device.
///
/// This stages the file (copies it into the engine's receive dir so the
/// session can stream it) and then sends a `Connect` command with the
/// attached `SendRequest`.
///
/// The function is `async` because it reads file metadata and copies the
/// file, but the actual network transfer happens in the background
/// inside the engine.
pub async fn send_file(
    cmd_tx: &mpsc::Sender<EngineCmd>,
    addr: &str,
    file_path: PathBuf,
) -> anyhow::Result<()> {
    let metadata = tokio::fs::metadata(&file_path).await?;
    anyhow::ensure!(metadata.is_file(), "Not a regular file");

    let size_bytes = metadata.len();
    let filename = file_path
        .file_name()
        .map_or_else(|| "unnamed".into(), |n| n.to_string_lossy().to_string());

    let transfer_id = generate_transfer_id();

    // Stage the file into the receive dir (the session reads from there).
    let staging_path = storage::receive_dir().join("send_file.txt");
    tokio::fs::copy(&file_path, &staging_path).await?;

    let send_request = SendRequest {
        transfer_id,
        file_path: staging_path,
        filename,
        size_bytes,
        sha256_hex: "not-computed".into(),
    };

    cmd_tx
        .send(EngineCmd::Connect {
            addr: addr.to_string(),
            send_request: Some(send_request),
        })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send Connect command: {e}"))?;

    Ok(())
}

// ─── Incoming: respond to offer ───────────────────────────────────────

/// Sends an accept or deny response for an incoming transfer offer.
pub async fn respond_to_offer(
    cmd_tx: &mpsc::Sender<EngineCmd>,
    session_id: u64,
    transfer_id: &str,
    accept: bool,
) -> anyhow::Result<()> {
    use waterdrop_engine::session::SessionCmd;

    cmd_tx
        .send(EngineCmd::SessionCmd {
            session_id,
            cmd: SessionCmd::RespondToOffer {
                transfer_id: transfer_id.to_string(),
                accept,
            },
        })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send RespondToOffer command: {e}"))?;

    Ok(())
}

// ─── Event listener ───────────────────────────────────────────────────

/// Spawns a background task that listens for [`EngineEvent`]s and
/// forwards [`BridgeEvent`] updates through an `mpsc` channel.
///
/// Returns the receiving end. The caller (a Dioxus component) should
/// drain it in a `use_future` / loop on the main thread and write
/// the values into the appropriate `Signal`s.
///
/// We use an `mpsc` channel instead of writing directly to a `Signal`
/// because `Signal` is `!Send` and cannot be moved into `tokio::spawn`.
pub fn spawn_event_listener(handle: &EngineHandle) -> mpsc::UnboundedReceiver<BridgeEvent> {
    let mut events_rx = handle.events_tx.subscribe();
    let (tx, rx) = mpsc::unbounded_channel::<BridgeEvent>();

    tokio::spawn(async move {
        // Track peer names per session so we can populate IncomingTransfer.
        let mut peer_names: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        // Track which sessions are inbound (server-side) vs outbound.
        // An inbound session will see TransferOffered; outbound will see TransferAccepted.
        // We determine this by whether we see TransferOffered for a session.
        let mut inbound_sessions: std::collections::HashSet<u64> = std::collections::HashSet::new();
        // Track the filename associated with an inbound transfer for progress display.
        let mut inbound_filenames: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();

        loop {
            match events_rx.recv().await {
                Ok(EngineEvent::SessionCreated { session_id, peer }) => {
                    debug!(session_id, peer = %peer, "Session created");
                    peer_names.insert(session_id, peer.clone());
                    // For outbound (sender) sessions we emit Connected;
                    // for inbound we wait for TransferOffered.
                    let _ = tx.send(BridgeEvent::Send(TransferState::Connected {
                        peer_name: peer,
                    }));
                }
                Ok(EngineEvent::SessionEvent { session_id, event }) => {
                    match classify_event(
                        session_id,
                        event,
                        &peer_names,
                        &mut inbound_sessions,
                        &mut inbound_filenames,
                    ) {
                        EventClassification::Send(state) => {
                            let _ = tx.send(BridgeEvent::Send(state));
                        }
                        EventClassification::Receive(state) => {
                            let _ = tx.send(BridgeEvent::Receive(state));
                        }
                        EventClassification::Both(send_state, recv_state) => {
                            let _ = tx.send(BridgeEvent::Send(send_state));
                            let _ = tx.send(BridgeEvent::Receive(recv_state));
                        }
                        EventClassification::ReceiveOrSend {
                            receive,
                            send,
                            is_inbound: inbound,
                        } => {
                            if inbound {
                                let _ = tx.send(BridgeEvent::Receive(receive));
                            } else {
                                let _ = tx.send(BridgeEvent::Send(send));
                            }
                        }
                        EventClassification::None => {}
                    }
                }
                Ok(EngineEvent::Error { message }) => {
                    warn!(error = %message, "Engine error");
                    let _ = tx.send(BridgeEvent::Send(TransferState::Error {
                        message: message.clone(),
                    }));
                    let _ = tx.send(BridgeEvent::Receive(ReceiveState::Error { message }));
                }
                Ok(EngineEvent::Accepting { addr }) => {
                    info!(addr = %addr, "Accepting connections");
                }
                Ok(EngineEvent::AcceptingStopped) => {
                    debug!("Accepting stopped");
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(n, "Missed engine events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("Engine event channel closed");
                    break;
                }
            }
        }
    });

    rx
}

/// Internal classification of engine session events.
#[allow(dead_code)]
enum EventClassification {
    /// This event is relevant to the outgoing (send) flow.
    Send(TransferState),
    /// This event is relevant to the incoming (receive) flow.
    Receive(ReceiveState),
    /// Emit both (unlikely in practice, but keeps the API complete).
    Both(TransferState, ReceiveState),
    /// Could be either depending on whether the session is inbound.
    ReceiveOrSend {
        receive: ReceiveState,
        send: TransferState,
        is_inbound: bool,
    },
    /// No UI update needed.
    None,
}

/// Classifies a session event into a send or receive state update.
fn classify_event(
    session_id: u64,
    event: SessionEvent,
    peer_names: &std::collections::HashMap<u64, String>,
    inbound_sessions: &mut std::collections::HashSet<u64>,
    inbound_filenames: &mut std::collections::HashMap<u64, String>,
) -> EventClassification {
    let is_inbound = inbound_sessions.contains(&session_id);

    match event {
        SessionEvent::Connected { peer_device_name } => {
            info!(session_id, peer = %peer_device_name, "Handshake complete");
            // Connected can happen for both directions. We already emit
            // a Send(Connected) from SessionCreated, so skip duplicates
            // for outbound. For inbound, we don't need to show anything
            // until TransferOffered arrives.
            EventClassification::None
        }

        SessionEvent::TransferOffered {
            transfer_id,
            filename,
            size_bytes,
        } => {
            info!(
                session_id,
                transfer_id = %transfer_id,
                filename = %filename,
                size_bytes,
                "Incoming transfer offer"
            );
            // Mark this session as inbound.
            inbound_sessions.insert(session_id);
            inbound_filenames.insert(session_id, filename.clone());

            let peer_name = peer_names
                .get(&session_id)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());

            EventClassification::Receive(ReceiveState::Offered {
                incoming: IncomingTransfer {
                    session_id,
                    transfer_id,
                    filename,
                    size_bytes,
                    peer_name,
                },
            })
        }

        SessionEvent::TransferAccepted { transfer_id } => {
            debug!(session_id, transfer_id = %transfer_id, "Transfer accepted by peer");
            // This is from our outgoing offer being accepted. Progress
            // events will follow.
            EventClassification::None
        }

        SessionEvent::TransferDenied { transfer_id } => {
            info!(session_id, transfer_id = %transfer_id, "Transfer denied by peer");
            EventClassification::Send(TransferState::Error {
                message: "Transfer was denied by the remote device".into(),
            })
        }

        SessionEvent::TransferProgress {
            transfer_id: _,
            bytes_transferred,
            total_bytes,
        } => {
            if is_inbound {
                let filename = inbound_filenames
                    .get(&session_id)
                    .cloned()
                    .unwrap_or_default();
                EventClassification::Receive(ReceiveState::Receiving {
                    filename,
                    bytes_received: bytes_transferred,
                    total_bytes,
                })
            } else {
                EventClassification::Send(TransferState::Sending {
                    filename: String::new(),
                    bytes_sent: bytes_transferred,
                    total_bytes,
                })
            }
        }

        SessionEvent::TransferComplete { transfer_id } => {
            info!(session_id, transfer_id = %transfer_id, "Transfer complete");
            if is_inbound {
                let filename = inbound_filenames
                    .remove(&session_id)
                    .unwrap_or_else(|| transfer_id.clone());
                let saved_path = storage::receive_dir().join(&filename);
                EventClassification::Receive(ReceiveState::Done {
                    filename,
                    saved_path,
                })
            } else {
                EventClassification::Send(TransferState::Done {
                    filename: transfer_id,
                })
            }
        }

        SessionEvent::Error { message } => {
            warn!(session_id, error = %message, "Session error");
            let ts = TransferState::Error {
                message: message.clone(),
            };
            let rs = ReceiveState::Error { message };
            EventClassification::ReceiveOrSend {
                receive: rs,
                send: ts,
                is_inbound,
            }
        }

        SessionEvent::Finished => {
            debug!(session_id, "Session finished");
            // Clean up tracking state.
            inbound_sessions.remove(&session_id);
            inbound_filenames.remove(&session_id);
            // Don't overwrite Done/Error with Idle — the UI resets
            // when the user dismisses the modal.
            EventClassification::None
        }
    }
}
