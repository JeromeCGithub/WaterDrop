use std::path::PathBuf;

use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use waterdrop_core::listener::{Connection, Connector, Listener, ListenerFactory};

use crate::session::{Role, SendRequest, Session, SessionCmd, SessionEvent};

// ── Engine commands (UI → engine) ───────────────────────────────────

/// Commands sent by the CLI / UI to control the engine.
#[derive(Clone, Debug)]
pub enum EngineCmd {
    /// Bind a listener on `addr` and start accepting connections (server).
    StartAccepting { addr: String },
    /// Stop accepting new connections (drop the listener).
    StopAccepting,
    /// Initiate an outbound connection to a remote peer (client) and
    /// optionally send a file immediately after the handshake.
    Connect {
        addr: String,
        send_request: Option<SendRequest>,
    },
    /// Send a command to a specific session identified by its ID.
    SessionCmd { session_id: u64, cmd: SessionCmd },
    /// Gracefully shut down the entire engine.
    ShutDown,
}

// ── Engine events (engine → UI) ─────────────────────────────────────

/// Events emitted by the engine for the CLI / UI to observe.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// The listener is bound and accepting connections on `addr`.
    Accepting { addr: String },
    /// The listener has been stopped.
    AcceptingStopped,
    /// A new session was created (inbound or outbound).
    SessionCreated { session_id: u64, peer: String },
    /// A session-level event, tagged with the session ID so the UI can
    /// route it to the right view / dialog.
    SessionEvent {
        session_id: u64,
        event: SessionEvent,
    },
    /// A non-fatal error occurred inside the engine.
    Error { message: String },
}

// ── Engine handle ───────────────────────────────────────────────────

/// Handle returned by [`Engine::start`].  Lets the caller send commands
/// and subscribe to events.
pub struct EngineHandle {
    pub cmd_tx: mpsc::Sender<EngineCmd>,
    pub events_tx: broadcast::Sender<EngineEvent>,
}

// ── Engine ──────────────────────────────────────────────────────────

/// Configuration shared by all sessions created by the engine.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Human-readable name for this device.
    pub device_name: String,
    /// Directory where received files are stored (server / receiver).
    pub receive_dir: PathBuf,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            device_name: "WaterDrop".into(),
            receive_dir: PathBuf::from("/tmp/waterdrop"),
        }
    }
}

/// The WaterDrop engine.
///
/// Supports **both** server mode (accept inbound connections) and client
/// mode (initiate outbound connections).  Each accepted or initiated
/// connection gets its own [`Session`] with a unique `session_id`.
///
/// The engine is generic over:
/// - `F: ListenerFactory` — creates listeners for server mode
/// - `K: Connector`       — creates outbound connections for client mode
///
/// Both are trait objects so the caller can plug in TCP, QUIC, or
/// in-memory transports without changing the engine code.
pub struct Engine;

impl Engine {
    /// Spawn the engine event loop and return a handle to control it.
    ///
    /// The engine starts idle — no listener is active and no connections
    /// are open until commands are received.
    pub fn start<F, K>(self, factory: F, connector: K, config: EngineConfig) -> EngineHandle
    where
        F: ListenerFactory,
        K: Connector,
    {
        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCmd>(32);
        let (events_tx, _) = broadcast::channel::<EngineEvent>(128);

        let events = events_tx.clone();

        info!("Spawning engine event loop");

        tokio::spawn(run_engine_loop::<F, K>(
            factory, connector, config, cmd_rx, events,
        ));

        debug!("Engine started successfully");
        EngineHandle { cmd_tx, events_tx }
    }
}

/// Internal bookkeeping for a spawned session.
struct ActiveSession {
    cmd_tx: mpsc::Sender<SessionCmd>,
}

/// The main engine event loop, extracted as a standalone async fn so that
/// the generic bounds don't infect `Engine` itself.
#[allow(clippy::too_many_lines)]
async fn run_engine_loop<F, K>(
    factory: F,
    connector: K,
    config: EngineConfig,
    mut cmd_rx: mpsc::Receiver<EngineCmd>,
    events: broadcast::Sender<EngineEvent>,
) where
    F: ListenerFactory,
    K: Connector,
{
    debug!("Engine event loop running");

    let mut listener: Option<F::L> = None;
    let mut next_session_id: u64 = 1;
    let mut sessions: Vec<(u64, ActiveSession)> = Vec::new();

    loop {
        tokio::select! {
            biased;

            // ── Commands ────────────────────────────────────────
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(EngineCmd::StartAccepting { addr }) => {
                        info!(addr = %addr, "Received StartAccepting command");
                        match factory.bind(&addr).await {
                            Ok(l) => {
                                let bound_addr = l.local_addr();
                                info!(addr = %bound_addr, "Listener bound");
                                listener = Some(l);
                                let _ = events.send(EngineEvent::Accepting {
                                    addr: bound_addr,
                                });
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to bind listener");
                                let _ = events.send(EngineEvent::Error {
                                    message: e.to_string(),
                                });
                            }
                        }
                    }

                    Some(EngineCmd::StopAccepting) => {
                        info!("Received StopAccepting command");
                        listener = None;
                        let _ = events.send(EngineEvent::AcceptingStopped);
                    }

                    Some(EngineCmd::Connect { addr, send_request }) => {
                        info!(addr = %addr, "Received Connect command");
                        match connector.connect(&addr).await {
                            Ok(conn) => {
                                let peer = Connection::peer(&conn);
                                let sid = next_session_id;
                                next_session_id += 1;

                                let _ = events.send(EngineEvent::SessionCreated {
                                    session_id: sid,
                                    peer,
                                });

                                let handle = Session::spawn(
                                    conn,
                                    Role::Client,
                                    config.device_name.clone(),
                                    send_request,
                                    config.receive_dir.clone(),
                                );

                                spawn_event_forwarder(sid, handle.event_rx, events.clone());

                                sessions.push((sid, ActiveSession {
                                    cmd_tx: handle.cmd_tx,
                                }));
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to connect");
                                let _ = events.send(EngineEvent::Error {
                                    message: e.to_string(),
                                });
                            }
                        }
                    }

                    Some(EngineCmd::SessionCmd { session_id, cmd }) => {
                        if let Some((_, session)) = sessions.iter().find(|(id, _)| *id == session_id) {
                            if let Err(e) = session.cmd_tx.send(cmd).await {
                                warn!(
                                    session_id = session_id,
                                    error = %e,
                                    "Failed to forward command to session"
                                );
                            }
                        } else {
                            warn!(session_id = session_id, "Session not found");
                        }
                    }

                    Some(EngineCmd::ShutDown) => {
                        info!("Received ShutDown command");
                        // Cancel all active sessions.
                        for (id, session) in &sessions {
                            debug!(session_id = id, "Cancelling session");
                            let _ = session.cmd_tx.send(SessionCmd::Cancel).await;
                        }
                        break;
                    }

                    None => {
                        debug!("Command channel closed, shutting down");
                        break;
                    }
                }
            }

            // ── Accept inbound connections ───────────────────────
            result = async {
                if let Some(l) = listener.as_mut() {
                    l.accept().await
                } else {
                    std::future::pending().await
                }
            }, if listener.is_some() => {
                match result {
                    Ok(conn) => {
                        let peer = Connection::peer(&conn);
                        let sid = next_session_id;
                        next_session_id += 1;

                        info!(session_id = sid, peer = %peer, "Connection accepted");
                        let _ = events.send(EngineEvent::SessionCreated {
                            session_id: sid,
                            peer,
                        });

                        let handle = Session::spawn(
                            conn,
                            Role::Server,
                            config.device_name.clone(),
                            None,
                            config.receive_dir.clone(),
                        );

                        spawn_event_forwarder(sid, handle.event_rx, events.clone());

                        sessions.push((sid, ActiveSession {
                            cmd_tx: handle.cmd_tx,
                        }));
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to accept connection");
                        let _ = events.send(EngineEvent::Error {
                            message: format!("{e}"),
                        });
                    }
                }
            }
        }
    }

    info!("Engine event loop stopped");
}

/// Spawns a background task that reads [`SessionEvent`]s from a session
/// and re-publishes them as [`EngineEvent::SessionEvent`]s on the engine
/// broadcast channel, tagged with the session ID.
fn spawn_event_forwarder(
    session_id: u64,
    mut event_rx: mpsc::Receiver<SessionEvent>,
    events_tx: broadcast::Sender<EngineEvent>,
) {
    tokio::spawn(async move {
        while let Some(ev) = event_rx.recv().await {
            let is_finished = matches!(ev, SessionEvent::Finished);
            let _ = events_tx.send(EngineEvent::SessionEvent {
                session_id,
                event: ev,
            });
            if is_finished {
                break;
            }
        }
        debug!(session_id = session_id, "Session event forwarder stopped");
    });
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::TcpStream;

    use super::*;
    use crate::tcp::{TcpConnector, TcpListenerFactory};

    /// Helper: start an engine with TCP transport.
    fn start_tcp_engine(config: EngineConfig) -> (EngineHandle, broadcast::Receiver<EngineEvent>) {
        let engine = Engine;
        let handle = engine.start(TcpListenerFactory, TcpConnector, config);
        let events_rx = handle.events_tx.subscribe();
        (handle, events_rx)
    }

    fn default_test_config() -> EngineConfig {
        let dir = tempfile::tempdir().expect("tempdir");
        EngineConfig {
            device_name: "TestDevice".into(),
            receive_dir: dir.path().to_path_buf(),
        }
    }

    /// Helper: wait for a specific event, with a timeout.
    async fn wait_for_event(
        rx: &mut broadcast::Receiver<EngineEvent>,
        matches_fn: impl Fn(&EngineEvent) -> bool,
    ) -> EngineEvent {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(ev) if matches_fn(&ev) => return ev,
                    Ok(_) => {}
                    Err(e) => panic!("event channel error: {e}"),
                }
            }
        })
        .await
        .expect("timed out waiting for event")
    }

    #[tokio::test]
    async fn when_start_accepting_expect_accepting_event() {
        let config = default_test_config();
        let (handle, mut events_rx) = start_tcp_engine(config);

        handle
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "127.0.0.1:0".into(),
            })
            .await
            .unwrap();

        let ev = wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::Accepting { .. })
        })
        .await;

        assert!(matches!(ev, EngineEvent::Accepting { .. }));
        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_stop_accepting_expect_stopped_event() {
        let config = default_test_config();
        let (handle, mut events_rx) = start_tcp_engine(config);

        handle
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "127.0.0.1:0".into(),
            })
            .await
            .unwrap();

        wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::Accepting { .. })
        })
        .await;

        handle.cmd_tx.send(EngineCmd::StopAccepting).await.unwrap();

        let ev = wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::AcceptingStopped)
        })
        .await;

        assert!(matches!(ev, EngineEvent::AcceptingStopped));
        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_client_connects_expect_session_created() {
        let config = default_test_config();
        let (handle, mut events_rx) = start_tcp_engine(config);

        handle
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "127.0.0.1:0".into(),
            })
            .await
            .unwrap();

        let EngineEvent::Accepting { addr } = wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::Accepting { .. })
        })
        .await
        else {
            unreachable!()
        };

        // A raw TCP connection triggers session creation on the server side.
        let _client = TcpStream::connect(&addr).await.unwrap();

        let ev = wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::SessionCreated { .. })
        })
        .await;

        assert!(matches!(ev, EngineEvent::SessionCreated { .. }));
        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_bind_fails_expect_error_event() {
        let config = default_test_config();
        let (handle, mut events_rx) = start_tcp_engine(config);

        handle
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "999.999.999.999:0".into(),
            })
            .await
            .unwrap();

        let ev = wait_for_event(&mut events_rx, |e| matches!(e, EngineEvent::Error { .. })).await;
        assert!(matches!(ev, EngineEvent::Error { .. }));

        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_connect_cmd_sent_expect_session_created_for_client() {
        let config = default_test_config();
        let (handle_server, mut events_server) = start_tcp_engine(config.clone());

        // Start a server so we have something to connect to.
        handle_server
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "127.0.0.1:0".into(),
            })
            .await
            .unwrap();

        let EngineEvent::Accepting { addr } = wait_for_event(&mut events_server, |e| {
            matches!(e, EngineEvent::Accepting { .. })
        })
        .await
        else {
            unreachable!()
        };

        // Start a separate engine as client.
        let (handle_client, mut events_client) = start_tcp_engine(config);

        handle_client
            .cmd_tx
            .send(EngineCmd::Connect {
                addr,
                send_request: None,
            })
            .await
            .unwrap();

        let ev = wait_for_event(&mut events_client, |e| {
            matches!(e, EngineEvent::SessionCreated { .. })
        })
        .await;

        assert!(matches!(ev, EngineEvent::SessionCreated { .. }));

        handle_client
            .cmd_tx
            .send(EngineCmd::ShutDown)
            .await
            .unwrap();
        handle_server
            .cmd_tx
            .send(EngineCmd::ShutDown)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn when_connect_cmd_with_send_request_expect_handshake_events() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        // Create a test file.
        let file_content = b"engine integration test data";
        let send_path = send_dir.path().join("send_file.txt");
        tokio::fs::write(&send_path, file_content).await.unwrap();

        let server_config = EngineConfig {
            device_name: "ServerDev".into(),
            receive_dir: recv_dir.path().to_path_buf(),
        };
        let client_config = EngineConfig {
            device_name: "ClientDev".into(),
            receive_dir: send_dir.path().to_path_buf(),
        };

        // Start server engine.
        let (handle_s, mut events_s) = start_tcp_engine(server_config);
        handle_s
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "127.0.0.1:0".into(),
            })
            .await
            .unwrap();

        let EngineEvent::Accepting { addr } = wait_for_event(&mut events_s, |e| {
            matches!(e, EngineEvent::Accepting { .. })
        })
        .await
        else {
            unreachable!()
        };

        // Start client engine with a send request.
        let send_req = SendRequest {
            transfer_id: "engine-xfer-1".into(),
            file_path: send_path,
            filename: "received_via_engine.txt".into(),
            size_bytes: file_content.len() as u64,
            sha256_hex: "unused".into(),
        };

        let (handle_c, mut events_c) = start_tcp_engine(client_config);
        handle_c
            .cmd_tx
            .send(EngineCmd::Connect {
                addr,
                send_request: Some(send_req),
            })
            .await
            .unwrap();

        // Client should get SessionCreated.
        wait_for_event(&mut events_c, |e| {
            matches!(e, EngineEvent::SessionCreated { .. })
        })
        .await;

        // Client should get a SessionEvent::Connected (from the handshake).
        let ev = wait_for_event(&mut events_c, |e| {
            matches!(
                e,
                EngineEvent::SessionEvent {
                    event: SessionEvent::Connected { .. },
                    ..
                }
            )
        })
        .await;
        assert!(matches!(
            ev,
            EngineEvent::SessionEvent {
                event: SessionEvent::Connected { .. },
                ..
            }
        ));

        // Server: wait for session created, then find the TransferOffered event.
        wait_for_event(&mut events_s, |e| {
            matches!(e, EngineEvent::SessionCreated { .. })
        })
        .await;

        let offered_ev = wait_for_event(&mut events_s, |e| {
            matches!(
                e,
                EngineEvent::SessionEvent {
                    event: SessionEvent::TransferOffered { .. },
                    ..
                }
            )
        })
        .await;

        // Extract session_id and transfer_id so we can accept.
        let EngineEvent::SessionEvent {
            session_id: sid,
            event:
                SessionEvent::TransferOffered {
                    transfer_id: tid, ..
                },
        } = offered_ev
        else {
            unreachable!()
        };

        // Server: accept the transfer.
        handle_s
            .cmd_tx
            .send(EngineCmd::SessionCmd {
                session_id: sid,
                cmd: SessionCmd::RespondToOffer {
                    transfer_id: tid,
                    accept: true,
                },
            })
            .await
            .unwrap();

        // Client: wait for TransferComplete.
        wait_for_event(&mut events_c, |e| {
            matches!(
                e,
                EngineEvent::SessionEvent {
                    event: SessionEvent::TransferComplete { .. },
                    ..
                }
            )
        })
        .await;

        // Server: wait for TransferComplete.
        wait_for_event(&mut events_s, |e| {
            matches!(
                e,
                EngineEvent::SessionEvent {
                    event: SessionEvent::TransferComplete { .. },
                    ..
                }
            )
        })
        .await;

        // Verify the file was received.
        let received = tokio::fs::read(recv_dir.path().join("received_via_engine.txt"))
            .await
            .unwrap();
        assert_eq!(received, file_content);

        handle_c.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
        handle_s.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_connect_fails_expect_error_event() {
        let config = default_test_config();
        let (handle, mut events_rx) = start_tcp_engine(config);

        handle
            .cmd_tx
            .send(EngineCmd::Connect {
                addr: "127.0.0.1:1".into(), // nothing listening
                send_request: None,
            })
            .await
            .unwrap();

        let ev = wait_for_event(&mut events_rx, |e| matches!(e, EngineEvent::Error { .. })).await;
        assert!(matches!(ev, EngineEvent::Error { .. }));

        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_session_cmd_to_unknown_session_then_no_panic() {
        let config = default_test_config();
        let (handle, _events_rx) = start_tcp_engine(config);

        // Send a command to a non-existent session — should not panic.
        handle
            .cmd_tx
            .send(EngineCmd::SessionCmd {
                session_id: 999,
                cmd: SessionCmd::Cancel,
            })
            .await
            .unwrap();

        // Give the engine a moment to process.
        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }
}
