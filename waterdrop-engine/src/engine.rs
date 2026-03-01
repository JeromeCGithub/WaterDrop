use std::path::PathBuf;

use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use waterdrop_core::transport::{Connection, Connector, Listener, ListenerFactory};

use crate::session::{Role, SendRequest, Session, SessionCmd, SessionEvent};

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

/// Handle returned by [`Engine::start`].  Lets the caller send commands
/// and subscribe to events.
pub struct EngineHandle {
    pub cmd_tx: mpsc::Sender<EngineCmd>,
    pub events_tx: broadcast::Sender<EngineEvent>,
}

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


                                let handle = Session::spawn(
                                    conn,
                                    Role::Client,
                                    config.device_name.clone(),
                                    config.receive_dir.clone(),
                                );

                                spawn_event_forwarder(sid, handle.event_rx, events.clone());

                                let _ = events.send(EngineEvent::SessionCreated {
                                    session_id: sid,
                                    peer,
                                });

                                if let Some(send_request) = send_request {
                                    let _ = handle.cmd_tx.send(SessionCmd::Transfer { req: send_request }).await;
                                }

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
