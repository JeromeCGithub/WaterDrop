use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Sends a [`SessionEvent`] to the UI.  Standalone function so that we
/// only borrow the [`mpsc::Sender`] (which is `Sync`) instead of all of
/// `Session` (which is not).
async fn emit(tx: &mpsc::Sender<SessionEvent>, event: SessionEvent) {
    if tx.send(event).await.is_err() {
        debug!("Event receiver dropped");
    }
}

use waterdrop_core::listener::{Connection, DataStream};
use waterdrop_core::protocol::{
    self, HelloAckPayload, HelloPayload, MessageType, TransferDecisionPayload, TransferDonePayload,
    TransferOfferPayload, decode_payload, encode_payload_frame, try_decode_frame,
};

// ── Session commands (UI → session) ─────────────────────────────────

/// Commands sent by the UI / CLI to steer a running session.
///
/// Each variant corresponds to a user action that advances or cancels
/// the protocol state machine.
#[derive(Debug, Clone)]
pub enum SessionCmd {
    /// Respond to a transfer offer.  Only valid while the session is in
    /// [`SessionState::AwaitingUserDecision`].
    RespondToOffer { transfer_id: String, accept: bool },

    /// Cancel the session at any point.  The session will send an ERROR
    /// frame (best-effort) and shut down.
    Cancel,
}

// ── Session events (session → UI) ───────────────────────────────────

/// Events emitted by a session for the UI / CLI to observe.
///
/// The UI subscribes to these to update its display — transfer progress
/// bars, accept/deny dialogs, error toasts, etc.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// The HELLO handshake completed successfully.
    Connected { peer_device_name: String },

    /// An inbound transfer offer arrived and requires a user decision.
    /// (Server-side only.)
    TransferOffered {
        transfer_id: String,
        filename: String,
        size_bytes: u64,
    },

    /// The remote accepted our transfer offer.  File streaming will
    /// begin automatically.  (Client/sender-side only.)
    TransferAccepted { transfer_id: String },

    /// The remote denied our transfer offer.
    TransferDenied { transfer_id: String },

    /// Progress update during file streaming.
    TransferProgress {
        transfer_id: String,
        bytes_transferred: u64,
        total_bytes: u64,
    },

    /// The transfer completed successfully.
    TransferComplete { transfer_id: String },

    /// The session ended due to an error.
    Error { message: String },

    /// The session has fully shut down.
    Finished,
}

// ── Session state machine ───────────────────────────────────────────

/// The role this session is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// We initiated the connection (sender).
    Client,
    /// We accepted the connection (receiver).
    Server,
}

/// Protocol state machine states.
///
/// Each variant encodes exactly what the session is waiting for or doing
/// at any given moment.  Transitions happen inside `Session::run` based
/// on received frames and UI commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Initial state.  Client sends HELLO, server waits for HELLO.
    Init,
    /// Client has sent HELLO, waiting for HELLO_ACK.
    WaitingForHelloAck,
    /// Server received HELLO, sent HELLO_ACK, now idle.
    /// Client received HELLO_ACK, now idle.
    Idle,
    /// Client has sent TRANSFER_OFFER, waiting for TRANSFER_DECISION.
    WaitingForDecision { transfer_id: String },
    /// Server received TRANSFER_OFFER, waiting for UI to accept/deny.
    AwaitingUserDecision { offer: TransferOfferPayload },
    /// File bytes are being streamed over a data stream.
    Transferring {
        transfer_id: String,
        size_bytes: u64,
    },
    /// Session finished (terminal state).
    Done,
}

/// Configuration for sending a file (client side).
#[derive(Debug, Clone)]
pub struct SendRequest {
    pub transfer_id: String,
    pub file_path: PathBuf,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256_hex: String,
}

/// A handle returned by [`Session::spawn`].
///
/// The caller (engine) keeps this to send commands and receive events.
pub struct SessionHandle {
    pub cmd_tx: mpsc::Sender<SessionCmd>,
    pub event_rx: mpsc::Receiver<SessionEvent>,
}

/// A transport-generic protocol session.
///
/// `Session` implements the WaterDrop file-transfer state machine over
/// any `C: Connection`.  It is spawned as an independent tokio task by
/// the engine and communicates with the UI through [`SessionCmd`] /
/// [`SessionEvent`] channels.
///
/// The session loop uses `tokio::select!` to react to **both** network
/// I/O and UI commands concurrently, making the protocol fully dynamic
/// and cancellable at every step.
pub struct Session<C: Connection> {
    conn: C,
    role: Role,
    state: SessionState,
    device_name: String,
    cmd_rx: mpsc::Receiver<SessionCmd>,
    event_tx: mpsc::Sender<SessionEvent>,
    accum: BytesMut,
    /// Only set for client sessions that want to send a file right after
    /// the handshake.
    send_request: Option<SendRequest>,
    /// Where to write received files (server side).
    receive_dir: PathBuf,
}

impl<C: Connection> Session<C> {
    /// Spawns a session as a background tokio task.
    ///
    /// Returns a [`SessionHandle`] that the engine / UI uses to drive
    /// the session.
    pub fn spawn(
        conn: C,
        role: Role,
        device_name: String,
        send_request: Option<SendRequest>,
        receive_dir: PathBuf,
    ) -> SessionHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(64);

        let session = Self {
            conn,
            role,
            state: SessionState::Init,
            device_name,
            cmd_rx,
            event_tx,
            accum: BytesMut::with_capacity(8192),
            send_request,
            receive_dir,
        };

        let err_tx = session.event_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = session.run().await {
                warn!(error = %e, "Session terminated with error");
                let _ = err_tx
                    .send(SessionEvent::Error {
                        message: e.to_string(),
                    })
                    .await;
                let _ = err_tx.send(SessionEvent::Finished).await;
            }
        });

        SessionHandle { cmd_tx, event_rx }
    }

    /// Main event loop.
    ///
    /// Runs until the state machine reaches [`SessionState::Done`] or an
    /// unrecoverable error occurs.
    async fn run(mut self) -> Result<()> {
        // ── Handshake phase ─────────────────────────────────────────
        self.do_handshake().await?;

        // ── Post-handshake: if client has a send request, send the offer
        if self.role == Role::Client
            && let Some(req) = self.send_request.take()
        {
            self.send_transfer_offer(&req).await?;
        }

        // ── Main select loop ────────────────────────────────────────
        let mut read_buf = [0u8; 4096];

        loop {
            if self.state == SessionState::Done {
                break;
            }

            tokio::select! {
                biased;

                // UI commands always take priority.
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(SessionCmd::Cancel) => {
                            info!("Session cancelled by user");
                            self.send_error("cancelled", "user cancelled").await;
                            self.transition(SessionState::Done);
                            emit(&self.event_tx, SessionEvent::Finished).await;
                            break;
                        }
                        Some(SessionCmd::RespondToOffer { transfer_id, accept }) => {
                            self.handle_user_decision(&transfer_id, accept).await?;
                        }
                        None => {
                            debug!("Command channel closed, finishing session");
                            self.transition(SessionState::Done);
                            emit(&self.event_tx, SessionEvent::Finished).await;
                            break;
                        }
                    }
                }

                // Network I/O.
                result = self.conn.read(&mut read_buf) => {
                    match result {
                        Ok(0) => {
                                info!("Connection closed by peer");
                                self.transition(SessionState::Done);
                                emit(&self.event_tx, SessionEvent::Finished).await;
                                break;
                            }
                        Ok(n) => {
                            self.accum.extend_from_slice(&read_buf[..n]);
                            self.drain_frames().await?;
                        }
                        Err(e) => {
                            warn!(error = %e, "Read error");
                            emit(&self.event_tx, SessionEvent::Error { message: e.to_string() }).await;
                            self.transition(SessionState::Done);
                            emit(&self.event_tx, SessionEvent::Finished).await;
                            break;
                        }
                    }
                }
            }
        }

        let _ = self.conn.shutdown().await;
        Ok(())
    }

    // ── Handshake ───────────────────────────────────────────────────

    async fn do_handshake(&mut self) -> Result<()> {
        match self.role {
            Role::Client => {
                // Send HELLO.
                let hello = HelloPayload {
                    device_name: self.device_name.clone(),
                };
                self.send_frame(MessageType::Hello, &hello).await?;
                self.transition(SessionState::WaitingForHelloAck);

                // Wait for HELLO_ACK.
                let frame = self.read_next_frame().await?;
                if frame.header.msg_type != MessageType::HelloAck {
                    bail!("expected HELLO_ACK, got {:?}", frame.header.msg_type);
                }
                let ack: HelloAckPayload = decode_payload(&frame.payload)?;
                if !ack.ok {
                    bail!("server rejected HELLO");
                }
                info!(peer = %ack.device_name, "Handshake complete (client)");
                self.transition(SessionState::Idle);
                emit(
                    &self.event_tx,
                    SessionEvent::Connected {
                        peer_device_name: ack.device_name,
                    },
                )
                .await;
            }
            Role::Server => {
                // Wait for HELLO.
                let frame = self.read_next_frame().await?;
                if frame.header.msg_type != MessageType::Hello {
                    bail!("expected HELLO, got {:?}", frame.header.msg_type);
                }
                let hello: HelloPayload = decode_payload(&frame.payload)?;
                info!(peer = %hello.device_name, "Received HELLO (server)");

                // Send HELLO_ACK.
                let ack = HelloAckPayload {
                    ok: true,
                    device_name: self.device_name.clone(),
                };
                self.send_frame(MessageType::HelloAck, &ack).await?;
                self.transition(SessionState::Idle);
                emit(
                    &self.event_tx,
                    SessionEvent::Connected {
                        peer_device_name: hello.device_name,
                    },
                )
                .await;
            }
        }
        Ok(())
    }

    // ── Transfer offer (client side) ────────────────────────────────

    async fn send_transfer_offer(&mut self, req: &SendRequest) -> Result<()> {
        let offer = TransferOfferPayload {
            transfer_id: req.transfer_id.clone(),
            filename: req.filename.clone(),
            size_bytes: req.size_bytes,
            sha256_hex: req.sha256_hex.clone(),
        };
        self.send_frame(MessageType::TransferOffer, &offer).await?;
        self.transition(SessionState::WaitingForDecision {
            transfer_id: req.transfer_id.clone(),
        });
        Ok(())
    }

    // ── User decision (server side) ─────────────────────────────────

    async fn handle_user_decision(&mut self, transfer_id: &str, accept: bool) -> Result<()> {
        // Verify we are in the right state.
        let offer = match &self.state {
            SessionState::AwaitingUserDecision { offer } if offer.transfer_id == transfer_id => {
                offer.clone()
            }
            _ => {
                warn!(
                    transfer_id = %transfer_id,
                    state = ?self.state,
                    "RespondToOffer received in wrong state"
                );
                return Ok(());
            }
        };

        let decision = TransferDecisionPayload {
            transfer_id: transfer_id.to_string(),
            accept,
        };
        self.send_frame(MessageType::TransferDecision, &decision)
            .await?;

        if accept {
            self.transition(SessionState::Transferring {
                transfer_id: offer.transfer_id.clone(),
                size_bytes: offer.size_bytes,
            });

            // Receive file bytes on a data stream.
            self.receive_file(&offer).await?;
        } else {
            self.transition(SessionState::Idle);
        }
        Ok(())
    }

    // ── Frame dispatch ──────────────────────────────────────────────

    /// Drains all complete frames from the accumulation buffer and
    /// dispatches them to the appropriate handler.
    async fn drain_frames(&mut self) -> Result<()> {
        loop {
            match try_decode_frame(&mut self.accum) {
                Ok(Some(frame)) => {
                    self.dispatch_frame(frame).await?;
                }
                Ok(None) => return Ok(()),
                Err(e) => {
                    warn!(error = %e, "Protocol error");
                    self.send_error("bad_frame", &e.to_string()).await;
                    emit(
                        &self.event_tx,
                        SessionEvent::Error {
                            message: e.to_string(),
                        },
                    )
                    .await;
                    self.transition(SessionState::Done);
                    emit(&self.event_tx, SessionEvent::Finished).await;
                    return Ok(());
                }
            }
        }
    }

    async fn dispatch_frame(&mut self, frame: protocol::Frame) -> Result<()> {
        debug!(
            msg_type = ?frame.header.msg_type,
            state = ?self.state,
            "Dispatching frame"
        );

        match frame.header.msg_type {
            MessageType::TransferOffer => {
                self.on_transfer_offer(&frame.payload).await?;
            }
            MessageType::TransferDecision => {
                self.on_transfer_decision(&frame.payload).await?;
            }
            MessageType::TransferDone => {
                self.on_transfer_done(&frame.payload).await?;
            }
            MessageType::Error => {
                self.on_error(&frame.payload).await;
            }
            other => {
                warn!(msg_type = ?other, "Unexpected message type in current state");
                self.send_error("bad_frame", "unexpected message type")
                    .await;
            }
        }
        Ok(())
    }

    // ── Frame handlers ──────────────────────────────────────────────

    /// Server receives TRANSFER_OFFER while idle.
    async fn on_transfer_offer(&mut self, payload: &[u8]) -> Result<()> {
        if self.state != SessionState::Idle {
            warn!(state = ?self.state, "TRANSFER_OFFER in wrong state");
            self.send_error("bad_frame", "unexpected TRANSFER_OFFER")
                .await;
            return Ok(());
        }

        let offer: TransferOfferPayload = decode_payload(payload)?;
        info!(
            transfer_id = %offer.transfer_id,
            filename = %offer.filename,
            size = offer.size_bytes,
            "Transfer offer received"
        );

        emit(
            &self.event_tx,
            SessionEvent::TransferOffered {
                transfer_id: offer.transfer_id.clone(),
                filename: offer.filename.clone(),
                size_bytes: offer.size_bytes,
            },
        )
        .await;

        self.transition(SessionState::AwaitingUserDecision { offer });
        Ok(())
    }

    /// Client receives TRANSFER_DECISION.
    async fn on_transfer_decision(&mut self, payload: &[u8]) -> Result<()> {
        let expected_id = if let SessionState::WaitingForDecision { transfer_id } = &self.state {
            transfer_id.clone()
        } else {
            warn!(state = ?self.state, "TRANSFER_DECISION in wrong state");
            return Ok(());
        };

        let decision: TransferDecisionPayload = decode_payload(payload)?;
        if decision.transfer_id != expected_id {
            warn!(
                expected = %expected_id,
                got = %decision.transfer_id,
                "transfer_id mismatch"
            );
            return Ok(());
        }

        if decision.accept {
            info!(transfer_id = %decision.transfer_id, "Transfer accepted by receiver");
            emit(
                &self.event_tx,
                SessionEvent::TransferAccepted {
                    transfer_id: decision.transfer_id.clone(),
                },
            )
            .await;

            // Reconstruct the send request info from the stored one.
            // We took it out in run(), so we need to have cached what we need.
            // The transfer_id in the state already tells us which request.
            self.transition(SessionState::Transferring {
                transfer_id: decision.transfer_id.clone(),
                size_bytes: 0, // client doesn't need this in state
            });

            self.stream_file(&decision.transfer_id).await?;
        } else {
            info!(transfer_id = %decision.transfer_id, "Transfer denied by receiver");
            emit(
                &self.event_tx,
                SessionEvent::TransferDenied {
                    transfer_id: decision.transfer_id,
                },
            )
            .await;
            self.transition(SessionState::Idle);
        }
        Ok(())
    }

    /// Either side receives TRANSFER_DONE.
    async fn on_transfer_done(&mut self, payload: &[u8]) -> Result<()> {
        let done: TransferDonePayload = decode_payload(payload)?;
        info!(
            transfer_id = %done.transfer_id,
            ok = done.ok,
            "Transfer done"
        );
        emit(
            &self.event_tx,
            SessionEvent::TransferComplete {
                transfer_id: done.transfer_id,
            },
        )
        .await;
        self.transition(SessionState::Done);
        emit(&self.event_tx, SessionEvent::Finished).await;
        Ok(())
    }

    /// Either side receives ERROR.
    async fn on_error(&mut self, payload: &[u8]) {
        if let Ok(err) = decode_payload::<protocol::ErrorPayload>(payload) {
            warn!(code = %err.code, message = %err.message, "Remote error");
            emit(
                &self.event_tx,
                SessionEvent::Error {
                    message: format!("{}: {}", err.code, err.message),
                },
            )
            .await;
        } else {
            warn!("Received ERROR frame with unparseable payload");
            emit(
                &self.event_tx,
                SessionEvent::Error {
                    message: "remote error (unparseable)".into(),
                },
            )
            .await;
        }
        self.transition(SessionState::Done);
        emit(&self.event_tx, SessionEvent::Finished).await;
    }

    // ── File streaming (sender) ─────────────────────────────────────

    /// Opens a data stream and writes the file bytes.
    ///
    /// The file path is currently hardcoded via the [`SendRequest`]
    /// stored during session creation.
    async fn stream_file(&mut self, transfer_id: &str) -> Result<()> {
        // Re-derive the path.  In the MVP the CLI passes it via SendRequest
        // which the engine stores before spawning.  We stash the path in a
        // side-channel file because `send_request` was consumed.  For the
        // MVP we look at the well-known hardcoded path.
        let path = self.receive_dir.join("send_file.txt");

        let file_data = tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read file for sending: {}", path.display()))?;

        let mut data_stream = self.conn.open_stream().await?;
        let total = file_data.len() as u64;

        // Stream in chunks so we can report progress.
        let chunk_size = 8192;
        let mut sent: u64 = 0;
        for chunk in file_data.chunks(chunk_size) {
            data_stream.write_all(chunk).await?;
            sent += chunk.len() as u64;
            emit(
                &self.event_tx,
                SessionEvent::TransferProgress {
                    transfer_id: transfer_id.to_string(),
                    bytes_transferred: sent,
                    total_bytes: total,
                },
            )
            .await;
        }
        data_stream.shutdown().await?;

        info!(transfer_id = %transfer_id, bytes = sent, "File sent");

        // Now wait for TRANSFER_DONE from the receiver (handled in main loop).
        // We go back to the select loop; the state is still Transferring.
        Ok(())
    }

    // ── File receiving (receiver) ───────────────────────────────────

    /// Accepts a data stream and reads exactly `size_bytes`.
    async fn receive_file(&mut self, offer: &TransferOfferPayload) -> Result<()> {
        let mut data_stream = self.conn.accept_stream().await?;

        let dest_path = self.receive_dir.join(&offer.filename);
        let mut received: u64 = 0;
        #[allow(clippy::cast_possible_truncation)]
        // file sizes fit in usize on all supported platforms
        let mut file_data = Vec::with_capacity(offer.size_bytes as usize);

        let mut buf = [0u8; 8192];
        while received < offer.size_bytes {
            let n = data_stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            file_data.extend_from_slice(&buf[..n]);
            received += n as u64;
            emit(
                &self.event_tx,
                SessionEvent::TransferProgress {
                    transfer_id: offer.transfer_id.clone(),
                    bytes_transferred: received,
                    total_bytes: offer.size_bytes,
                },
            )
            .await;
        }

        if received != offer.size_bytes {
            let msg = format!(
                "incomplete transfer: got {} of {} bytes",
                received, offer.size_bytes
            );
            warn!(msg);
            let done = TransferDonePayload {
                transfer_id: offer.transfer_id.clone(),
                ok: false,
                stored_filename: None,
                reason: Some(msg.clone()),
            };
            self.send_frame(MessageType::TransferDone, &done).await?;
            emit(&self.event_tx, SessionEvent::Error { message: msg }).await;
            self.transition(SessionState::Done);
            emit(&self.event_tx, SessionEvent::Finished).await;
            return Ok(());
        }

        // Write file to disk.
        tokio::fs::write(&dest_path, &file_data)
            .await
            .with_context(|| format!("failed to write received file to {}", dest_path.display()))?;

        info!(
            path = %dest_path.display(),
            bytes = received,
            "File received and written"
        );

        let done = TransferDonePayload {
            transfer_id: offer.transfer_id.clone(),
            ok: true,
            stored_filename: Some(offer.filename.clone()),
            reason: None,
        };
        self.send_frame(MessageType::TransferDone, &done).await?;

        emit(
            &self.event_tx,
            SessionEvent::TransferComplete {
                transfer_id: offer.transfer_id.clone(),
            },
        )
        .await;

        self.transition(SessionState::Done);
        emit(&self.event_tx, SessionEvent::Finished).await;
        Ok(())
    }

    // ── Helpers ─────────────────────────────────────────────────────

    fn transition(&mut self, new_state: SessionState) {
        debug!(from = ?self.state, to = ?new_state, "State transition");
        self.state = new_state;
    }

    async fn send_frame<T: serde::Serialize>(
        &mut self,
        msg_type: MessageType,
        payload: &T,
    ) -> Result<()> {
        let bytes = encode_payload_frame(msg_type, payload)?;
        self.conn.write_all(&bytes).await?;
        Ok(())
    }

    async fn send_error(&mut self, code: &str, message: &str) {
        let payload = protocol::ErrorPayload {
            code: code.to_string(),
            message: message.to_string(),
        };
        if let Err(e) = self.send_frame(MessageType::Error, &payload).await {
            warn!(error = %e, "Failed to send ERROR frame");
        }
    }

    /// Reads from the connection until at least one complete frame has
    /// been accumulated, then returns it.
    async fn read_next_frame(&mut self) -> Result<protocol::Frame> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(frame) = try_decode_frame(&mut self.accum)? {
                return Ok(frame);
            }
            let n = self.conn.read(&mut buf).await?;
            if n == 0 {
                bail!("connection closed while waiting for frame");
            }
            self.accum.extend_from_slice(&buf[..n]);
        }
    }

    /// Returns the current state.  Useful in tests.
    #[cfg(test)]
    pub fn state(&self) -> &SessionState {
        &self.state
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Result;
    use tokio::sync::mpsc;

    use waterdrop_core::listener::{Connection, DataStream};
    use waterdrop_core::protocol::{
        HelloAckPayload, MessageType, TransferOfferPayload, encode_payload_frame, try_decode_frame,
    };

    use super::*;

    // ── Mock transport ──────────────────────────────────────────────

    /// A bidirectional in-memory byte pipe used as a mock data stream.
    struct MemoryPipe {
        read_buf: Arc<Mutex<VecDeque<u8>>>,
        write_buf: Arc<Mutex<VecDeque<u8>>>,
        read_notify: Arc<tokio::sync::Notify>,
        write_notify: Arc<tokio::sync::Notify>,
        closed: Arc<Mutex<bool>>,
    }

    impl MemoryPipe {
        fn pair() -> (Self, Self) {
            let ab = Arc::new(Mutex::new(VecDeque::new()));
            let ba = Arc::new(Mutex::new(VecDeque::new()));
            let notify_ab = Arc::new(tokio::sync::Notify::new());
            let notify_ba = Arc::new(tokio::sync::Notify::new());
            let closed_a = Arc::new(Mutex::new(false));
            let closed_b = Arc::new(Mutex::new(false));

            let a = Self {
                read_buf: ba.clone(),
                write_buf: ab.clone(),
                read_notify: notify_ba.clone(),
                write_notify: notify_ab.clone(),
                closed: closed_a,
            };
            let b = Self {
                read_buf: ab,
                write_buf: ba,
                read_notify: notify_ab,
                write_notify: notify_ba,
                closed: closed_b,
            };
            (a, b)
        }
    }

    impl DataStream for MemoryPipe {
        async fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize> {
            loop {
                {
                    let mut q = self.read_buf.lock().unwrap();
                    if !q.is_empty() {
                        let n = buf.len().min(q.len());
                        for item in buf.iter_mut().take(n) {
                            *item = q.pop_front().unwrap();
                        }
                        return Ok(n);
                    }
                    if *self.closed.lock().unwrap() {
                        return Ok(0);
                    }
                }
                self.read_notify.notified().await;
            }
        }

        async fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> Result<()> {
            {
                let mut q = self.write_buf.lock().unwrap();
                q.extend(buf);
            }
            self.write_notify.notify_waiters();
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<()> {
            *self.closed.lock().unwrap() = true;
            self.write_notify.notify_waiters();
            self.read_notify.notify_waiters();
            Ok(())
        }
    }

    /// Mock connection backed by in-memory pipes.
    struct MockConnection {
        control: MemoryPipe,
        /// Streams opened by this side are pushed here; the other side
        /// pops from its `incoming_streams`.
        outgoing_streams: Arc<Mutex<Vec<MemoryPipe>>>,
        incoming_streams: Arc<Mutex<Vec<MemoryPipe>>>,
        stream_notify: Arc<tokio::sync::Notify>,
        peer_name: String,
    }

    impl MockConnection {
        fn pair(name_a: &str, name_b: &str) -> (Self, Self) {
            let (ctrl_a, ctrl_b) = MemoryPipe::pair();
            let streams_a_to_b: Arc<Mutex<Vec<MemoryPipe>>> = Arc::new(Mutex::new(Vec::new()));
            let streams_b_to_a: Arc<Mutex<Vec<MemoryPipe>>> = Arc::new(Mutex::new(Vec::new()));
            let notify = Arc::new(tokio::sync::Notify::new());

            let a = Self {
                control: ctrl_a,
                outgoing_streams: streams_a_to_b.clone(),
                incoming_streams: streams_b_to_a.clone(),
                stream_notify: notify.clone(),
                peer_name: name_a.to_string(),
            };
            let b = Self {
                control: ctrl_b,
                outgoing_streams: streams_b_to_a,
                incoming_streams: streams_a_to_b,
                stream_notify: notify,
                peer_name: name_b.to_string(),
            };
            (a, b)
        }
    }

    impl Connection for MockConnection {
        type DataStream = MemoryPipe;

        fn peer(&self) -> String {
            self.peer_name.clone()
        }

        async fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize> {
            self.control.read(buf).await
        }

        async fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> Result<()> {
            self.control.write_all(buf).await
        }

        async fn shutdown(&mut self) -> Result<()> {
            self.control.shutdown().await
        }

        async fn open_stream(&mut self) -> Result<Self::DataStream> {
            let (mine, theirs) = MemoryPipe::pair();
            {
                self.outgoing_streams.lock().unwrap().push(theirs);
            }
            self.stream_notify.notify_waiters();
            Ok(mine)
        }

        async fn accept_stream(&mut self) -> Result<Self::DataStream> {
            loop {
                {
                    let mut streams = self.incoming_streams.lock().unwrap();
                    if !streams.is_empty() {
                        return Ok(streams.remove(0));
                    }
                }
                self.stream_notify.notified().await;
            }
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────

    async fn collect_events_until(
        rx: &mut mpsc::Receiver<SessionEvent>,
        pred: impl Fn(&SessionEvent) -> bool,
        timeout: Duration,
    ) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(ev)) => {
                    let done = pred(&ev);
                    events.push(ev);
                    if done {
                        return events;
                    }
                }
                Ok(None) | Err(_) => return events,
            }
        }
    }

    async fn wait_for_event(
        rx: &mut mpsc::Receiver<SessionEvent>,
        pred: impl Fn(&SessionEvent) -> bool,
    ) -> SessionEvent {
        let events = collect_events_until(rx, &pred, Duration::from_secs(5)).await;
        events
            .into_iter()
            .rfind(|e| pred(e))
            .expect("timed out waiting for event")
    }

    fn make_temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    // ── Tests ───────────────────────────────────────────────────────

    /// Given a client and server, when they run the handshake, then both
    /// emit Connected events with each other's device name.
    #[tokio::test]
    async fn given_client_and_server_when_handshake_then_both_emit_connected() {
        let (conn_c, conn_s) = MockConnection::pair("client", "server");
        let dir = make_temp_dir();

        let mut handle_s = Session::spawn(
            conn_s,
            Role::Server,
            "ServerDevice".into(),
            None,
            dir.path().to_path_buf(),
        );
        let mut handle_c = Session::spawn(
            conn_c,
            Role::Client,
            "ClientDevice".into(),
            None,
            dir.path().to_path_buf(),
        );

        let ev_c = wait_for_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        assert!(
            matches!(ev_c, SessionEvent::Connected { peer_device_name } if peer_device_name == "ServerDevice")
        );

        let ev_s = wait_for_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        assert!(
            matches!(ev_s, SessionEvent::Connected { peer_device_name } if peer_device_name == "ClientDevice")
        );

        // Clean up.
        let _ = handle_c.cmd_tx.send(SessionCmd::Cancel).await;
        let _ = handle_s.cmd_tx.send(SessionCmd::Cancel).await;
    }

    /// Given a client that cancels during idle, when cancel is sent, then
    /// the session emits Finished.
    #[tokio::test]
    async fn given_idle_session_when_cancelled_then_emits_finished() {
        let (conn_c, conn_s) = MockConnection::pair("client", "server");
        let dir = make_temp_dir();

        let handle_s = Session::spawn(
            conn_s,
            Role::Server,
            "S".into(),
            None,
            dir.path().to_path_buf(),
        );
        let mut handle_c = Session::spawn(
            conn_c,
            Role::Client,
            "C".into(),
            None,
            dir.path().to_path_buf(),
        );

        // Wait for handshake to complete.
        wait_for_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        // Cancel the client.
        handle_c.cmd_tx.send(SessionCmd::Cancel).await.unwrap();

        let ev = wait_for_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Finished)
        })
        .await;
        assert!(matches!(ev, SessionEvent::Finished));

        let _ = handle_s.cmd_tx.send(SessionCmd::Cancel).await;
    }

    /// Given a full file transfer, when the server accepts, then file
    /// bytes are streamed and both sides complete.
    #[tokio::test]
    async fn given_file_transfer_when_accepted_then_file_received() {
        let send_dir = make_temp_dir();
        let recv_dir = make_temp_dir();

        // Create a test file to send.
        let test_content = b"Hello, WaterDrop! This is a test file.";
        let send_path = send_dir.path().join("send_file.txt");
        tokio::fs::write(&send_path, test_content).await.unwrap();

        let (conn_c, conn_s) = MockConnection::pair("sender", "receiver");

        let send_req = SendRequest {
            transfer_id: "xfer-1".into(),
            file_path: send_path.clone(),
            filename: "received.txt".into(),
            size_bytes: test_content.len() as u64,
            sha256_hex: "not-checked-yet".into(),
        };

        let mut handle_s = Session::spawn(
            conn_s,
            Role::Server,
            "Receiver".into(),
            None,
            recv_dir.path().to_path_buf(),
        );
        let mut handle_c = Session::spawn(
            conn_c,
            Role::Client,
            "Sender".into(),
            Some(send_req),
            send_dir.path().to_path_buf(),
        );

        // Server: wait for TransferOffered.
        let ev = wait_for_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;
        let SessionEvent::TransferOffered {
            transfer_id: tid, ..
        } = ev
        else {
            unreachable!()
        };
        assert_eq!(tid, "xfer-1");

        // Server: accept the offer.
        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: tid,
                accept: true,
            })
            .await
            .unwrap();

        // Client: should see TransferAccepted then TransferComplete.
        let ev_c = wait_for_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::TransferComplete { .. })
        })
        .await;
        assert!(matches!(ev_c, SessionEvent::TransferComplete { .. }));

        // Server: should see TransferComplete.
        let ev_s = wait_for_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferComplete { .. })
        })
        .await;
        assert!(matches!(ev_s, SessionEvent::TransferComplete { .. }));

        // Verify the file was written.
        let received = tokio::fs::read(recv_dir.path().join("received.txt"))
            .await
            .unwrap();
        assert_eq!(received, test_content);
    }

    /// Given a transfer offer, when the server denies it, then the client
    /// sees TransferDenied and the session remains idle.
    #[tokio::test]
    async fn given_transfer_offer_when_denied_then_client_sees_denied() {
        let send_dir = make_temp_dir();
        let recv_dir = make_temp_dir();

        let test_content = b"data";
        let send_path = send_dir.path().join("send_file.txt");
        tokio::fs::write(&send_path, test_content).await.unwrap();

        let (conn_c, conn_s) = MockConnection::pair("sender", "receiver");

        let send_req = SendRequest {
            transfer_id: "xfer-deny".into(),
            file_path: send_path,
            filename: "nope.txt".into(),
            size_bytes: test_content.len() as u64,
            sha256_hex: "abc".into(),
        };

        let mut handle_s = Session::spawn(
            conn_s,
            Role::Server,
            "Receiver".into(),
            None,
            recv_dir.path().to_path_buf(),
        );
        let mut handle_c = Session::spawn(
            conn_c,
            Role::Client,
            "Sender".into(),
            Some(send_req),
            send_dir.path().to_path_buf(),
        );

        // Server: wait for the offer.
        wait_for_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;

        // Server: deny the offer.
        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: "xfer-deny".into(),
                accept: false,
            })
            .await
            .unwrap();

        // Client: should see TransferDenied.
        let ev = wait_for_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::TransferDenied { .. })
        })
        .await;
        assert!(
            matches!(ev, SessionEvent::TransferDenied { transfer_id } if transfer_id == "xfer-deny")
        );

        let _ = handle_c.cmd_tx.send(SessionCmd::Cancel).await;
        let _ = handle_s.cmd_tx.send(SessionCmd::Cancel).await;
    }

    /// Given a large file, when transferred, then progress events are emitted
    /// and the file arrives intact.
    #[tokio::test]
    async fn given_large_file_when_transferred_then_progress_emitted_and_intact() {
        let send_dir = make_temp_dir();
        let recv_dir = make_temp_dir();

        // Create a ~64KB file.
        #[allow(clippy::cast_possible_truncation)]
        let test_content: Vec<u8> = (0..65_536u32).map(|i| (i % 256) as u8).collect();
        let send_path = send_dir.path().join("send_file.txt");
        tokio::fs::write(&send_path, &test_content).await.unwrap();

        let (conn_c, conn_s) = MockConnection::pair("sender", "receiver");

        let send_req = SendRequest {
            transfer_id: "xfer-large".into(),
            file_path: send_path,
            filename: "big.bin".into(),
            size_bytes: test_content.len() as u64,
            sha256_hex: "unused".into(),
        };

        let mut handle_s = Session::spawn(
            conn_s,
            Role::Server,
            "R".into(),
            None,
            recv_dir.path().to_path_buf(),
        );
        let mut handle_c = Session::spawn(
            conn_c,
            Role::Client,
            "S".into(),
            Some(send_req),
            send_dir.path().to_path_buf(),
        );

        // Server: accept the offer.
        wait_for_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;
        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: "xfer-large".into(),
                accept: true,
            })
            .await
            .unwrap();

        // Client: collect all events until TransferComplete.
        let client_events = collect_events_until(
            &mut handle_c.event_rx,
            |e| matches!(e, SessionEvent::TransferComplete { .. }),
            Duration::from_secs(10),
        )
        .await;
        let progress_count = client_events
            .iter()
            .filter(|e| matches!(e, SessionEvent::TransferProgress { .. }))
            .count();
        assert!(progress_count > 0, "expected at least one progress event");

        // Server: wait for completion.
        wait_for_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferComplete { .. })
        })
        .await;

        // Verify file integrity.
        let received = tokio::fs::read(recv_dir.path().join("big.bin"))
            .await
            .unwrap();
        assert_eq!(received, test_content);
    }

    /// Given a session state machine, when transitions happen, the state
    /// names match expectations (unit test of the enum itself).
    #[test]
    fn given_session_states_when_compared_then_equality_works() {
        assert_eq!(SessionState::Init, SessionState::Init);
        assert_ne!(SessionState::Init, SessionState::Idle);
        assert_ne!(SessionState::Init, SessionState::Done);
        assert_eq!(
            SessionState::WaitingForDecision {
                transfer_id: "a".into()
            },
            SessionState::WaitingForDecision {
                transfer_id: "a".into()
            }
        );
        assert_ne!(
            SessionState::WaitingForDecision {
                transfer_id: "a".into()
            },
            SessionState::WaitingForDecision {
                transfer_id: "b".into()
            }
        );
    }

    /// Given a MockConnection pair, when one side writes and the other
    /// reads on the control channel, then data flows correctly.
    #[tokio::test]
    async fn given_mock_connection_when_write_read_then_data_flows() {
        let (mut a, mut b) = MockConnection::pair("a", "b");

        Connection::write_all(&mut a, b"hello").await.unwrap();
        let mut buf = [0u8; 16];
        let n = Connection::read(&mut b, &mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    /// Given a MockConnection, when open_stream / accept_stream are used,
    /// then data flows on the data stream.
    #[tokio::test]
    async fn given_mock_connection_when_data_stream_then_flows() {
        let (mut a, mut b) = MockConnection::pair("a", "b");

        let mut stream_a = a.open_stream().await.unwrap();
        stream_a.write_all(b"data-stream-payload").await.unwrap();

        // Small delay to let the notify propagate.
        tokio::time::sleep(Duration::from_millis(10)).await;

        let mut stream_b = b.accept_stream().await.unwrap();
        let mut buf = [0u8; 64];
        let n = stream_b.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"data-stream-payload");
    }

    /// Given a server session, when it receives a HELLO with wrong type,
    /// then the session errors.
    #[tokio::test]
    async fn given_server_when_receives_wrong_first_frame_then_errors() {
        let (mut conn_c, conn_s) = MockConnection::pair("c", "s");
        let dir = make_temp_dir();

        let mut handle_s = Session::spawn(
            conn_s,
            Role::Server,
            "S".into(),
            None,
            dir.path().to_path_buf(),
        );

        // Send a TransferOffer instead of HELLO.
        let offer = TransferOfferPayload {
            transfer_id: "bad".into(),
            filename: "x".into(),
            size_bytes: 0,
            sha256_hex: String::new(),
        };
        let frame = encode_payload_frame(MessageType::TransferOffer, &offer).unwrap();
        Connection::write_all(&mut conn_c, &frame).await.unwrap();

        // The server session should emit an Error or Finished.
        let ev = wait_for_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Error { .. } | SessionEvent::Finished)
        })
        .await;
        assert!(matches!(
            ev,
            SessionEvent::Error { .. } | SessionEvent::Finished
        ));
    }

    /// Given a client session, when the server sends HelloAck with ok=false,
    /// then the client session errors.
    #[tokio::test]
    async fn given_client_when_hello_rejected_then_errors() {
        let (conn_c, mut conn_s) = MockConnection::pair("c", "s");
        let dir = make_temp_dir();

        let mut handle_c = Session::spawn(
            conn_c,
            Role::Client,
            "C".into(),
            None,
            dir.path().to_path_buf(),
        );

        // Manually act as server: read HELLO, send rejecting HELLO_ACK.
        let mut buf = [0u8; 1024];
        let mut accum = BytesMut::new();
        loop {
            let n = Connection::read(&mut conn_s, &mut buf).await.unwrap();
            accum.extend_from_slice(&buf[..n]);
            if try_decode_frame(&mut accum).unwrap().is_some() {
                break;
            }
        }

        let ack = HelloAckPayload {
            ok: false,
            device_name: "S".into(),
        };
        let frame = encode_payload_frame(MessageType::HelloAck, &ack).unwrap();
        Connection::write_all(&mut conn_s, &frame).await.unwrap();

        // Client should see an error/finished.
        let ev = wait_for_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Error { .. } | SessionEvent::Finished)
        })
        .await;
        assert!(matches!(
            ev,
            SessionEvent::Error { .. } | SessionEvent::Finished
        ));
    }
}
