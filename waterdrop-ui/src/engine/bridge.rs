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

/// The status of a file transfer that the UI can display.
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

/// Generates a unique transfer ID from the current timestamp.
fn generate_transfer_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("xfer-{ts}")
}

/// Starts the WaterDrop engine on the Tokio runtime.
///
/// - Binds a QUIC listener on `listen_addr` so we can also *receive*
///   files (not exposed in the UI yet, but the engine supports it).
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

/// Spawns a background task that listens for [`EngineEvent`]s and
/// forwards [`TransferState`] updates through an `mpsc` channel.
///
/// Returns the receiving end.  The caller (a Dioxus component) should
/// drain it in a `use_future` / loop on the main thread and write
/// the values into a `Signal<TransferState>`.
///
/// We use an `mpsc` channel instead of writing directly to a `Signal`
/// because `Signal` is `!Send` and cannot be moved into `tokio::spawn`.
pub fn spawn_event_listener(handle: &EngineHandle) -> mpsc::UnboundedReceiver<TransferState> {
    let mut events_rx = handle.events_tx.subscribe();
    let (tx, rx) = mpsc::unbounded_channel::<TransferState>();

    tokio::spawn(async move {
        loop {
            match events_rx.recv().await {
                Ok(EngineEvent::SessionCreated { session_id, peer }) => {
                    debug!(session_id, peer = %peer, "Session created");
                    let _ = tx.send(TransferState::Connected { peer_name: peer });
                }
                Ok(EngineEvent::SessionEvent { session_id, event }) => {
                    if let Some(state) = session_event_to_state(session_id, event) {
                        let _ = tx.send(state);
                    }
                }
                Ok(EngineEvent::Error { message }) => {
                    warn!(error = %message, "Engine error");
                    let _ = tx.send(TransferState::Error { message });
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

/// Converts a [`SessionEvent`] into an optional [`TransferState`] update.
///
/// Returns `None` for events that don't require a UI state change.
fn session_event_to_state(session_id: u64, event: SessionEvent) -> Option<TransferState> {
    match event {
        SessionEvent::Connected { peer_device_name } => {
            info!(session_id, peer = %peer_device_name, "Handshake complete");
            Some(TransferState::Connected {
                peer_name: peer_device_name,
            })
        }
        SessionEvent::TransferAccepted { transfer_id } => {
            debug!(session_id, transfer_id = %transfer_id, "Transfer accepted by peer");
            // Progress events will follow; keep the current state.
            None
        }
        SessionEvent::TransferDenied { transfer_id } => {
            info!(session_id, transfer_id = %transfer_id, "Transfer denied by peer");
            Some(TransferState::Error {
                message: "Transfer was denied by the remote device".into(),
            })
        }
        SessionEvent::TransferProgress {
            transfer_id: _,
            bytes_transferred,
            total_bytes,
        } => Some(TransferState::Sending {
            filename: String::new(),
            bytes_sent: bytes_transferred,
            total_bytes,
        }),
        SessionEvent::TransferComplete { transfer_id } => {
            info!(session_id, transfer_id = %transfer_id, "Transfer complete");
            Some(TransferState::Done {
                filename: transfer_id,
            })
        }
        SessionEvent::TransferOffered {
            transfer_id: _,
            filename,
            size_bytes: _,
        } => {
            // We are the sender UI — we don't handle incoming offers
            // in this version, but log it for debugging.
            debug!(session_id, filename = %filename, "Received incoming offer (ignored)");
            None
        }
        SessionEvent::Error { message } => {
            warn!(session_id, error = %message, "Session error");
            Some(TransferState::Error { message })
        }
        SessionEvent::Finished => {
            debug!(session_id, "Session finished");
            // Return None — we don't want to overwrite Done/Error with Idle.
            // The UI can reset to Idle when the user dismisses the modal.
            None
        }
    }
}
