use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use waterdrop_core::listener::{Connection, Listener, ListenerFactory};

use crate::session::SessionHandler;

/// Commands sent by the CLI / UI to control the engine.
#[derive(Clone, Debug)]
pub enum EngineCmd {
    /// Bind a listener on `addr` and start accepting connections.
    StartAccepting { addr: String },
    /// Stop accepting new connections (drop the listener).
    StopAccepting,
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
    /// A new inbound connection was accepted from `peer`.
    ConnectionAccepted { peer: String },
    /// A non-fatal error occurred inside the engine.
    Error { message: String },
}

/// Handle returned by [`Engine::start`]. Lets the caller send commands and
/// subscribe to events.
pub struct EngineHandle {
    pub cmd_tx: mpsc::Sender<EngineCmd>,
    pub events_tx: broadcast::Sender<EngineEvent>,
}

#[derive(Default)]
pub struct Engine;

impl Engine {
    /// Spawn the engine event loop and return a handle to control it.
    ///
    /// The engine starts idle — no listener is active until a
    /// [`EngineCmd::StartAccepting`] command is received.
    ///
    /// Every accepted connection is handed off to `handler` in its own
    /// spawned task, keeping the accept loop free.
    pub fn start<F, H>(self, factory: F, handler: Arc<H>) -> EngineHandle
    where
        F: ListenerFactory,
        H: SessionHandler<<F::L as Listener>::Conn>,
    {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<EngineCmd>(32);
        let (events_tx, _) = broadcast::channel::<EngineEvent>(64);

        let events = events_tx.clone();

        info!("Spawning engine event loop");

        tokio::spawn(async move {
            debug!("Engine event loop running");

            let mut listener: Option<F::L> = None;

            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(EngineCmd::StartAccepting { addr }) => {
                                info!(addr = %addr, "Received StartAccepting command");
                                match factory.bind(&addr).await {
                                    Ok(l) => {
                                        let bound_addr = l.local_addr();
                                        info!(addr = %bound_addr, "Listener bound, accepting connections");
                                        listener = Some(l);
                                        let _ = events.send(EngineEvent::Accepting { addr: bound_addr });
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "Failed to bind listener");
                                        let _ = events.send(EngineEvent::Error { message: e.to_string() });
                                    }
                                }
                            }
                            Some(EngineCmd::StopAccepting) => {
                                info!("Received StopAccepting command");
                                listener = None;
                                let _ = events.send(EngineEvent::AcceptingStopped);
                            }
                            Some(EngineCmd::ShutDown) => {
                                info!("Received ShutDown command");
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
                    } => {
                        let conn: anyhow::Result<<F::L as Listener>::Conn> = result;
                        match conn {
                            Ok(c) => {
                                let peer = Connection::peer(&c);
                                info!(peer = %peer, "Connection accepted");
                                let _ = events.send(EngineEvent::ConnectionAccepted { peer });

                                let h = Arc::clone(&handler);
                                tokio::spawn(async move {
                                    h.handle(c).await;
                                });
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to accept connection");
                                let _ = events.send(EngineEvent::Error { message: format!("{e}") });
                            }
                        }
                    }
                }
            }

            info!("Engine event loop stopped");
        });

        debug!("Engine started successfully");
        EngineHandle { cmd_tx, events_tx }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::net::TcpStream;

    use super::*;
    use crate::tcp::{TcpConnection, TcpListenerFactory};

    /// A mock session handler that counts how many connections it received.
    struct MockHandler {
        count: AtomicUsize,
        notify: tokio::sync::Notify,
    }

    impl MockHandler {
        fn new() -> Self {
            Self {
                count: AtomicUsize::new(0),
                notify: tokio::sync::Notify::new(),
            }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }

        async fn wait_for_count(&self, expected: usize) {
            while self.count.load(Ordering::SeqCst) < expected {
                self.notify.notified().await;
            }
        }
    }

    impl SessionHandler<TcpConnection> for MockHandler {
        async fn handle(&self, _conn: TcpConnection) {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    /// Helper: start an engine with a mock handler and subscribe to events.
    fn start_engine(handler: Arc<MockHandler>) -> (EngineHandle, broadcast::Receiver<EngineEvent>) {
        let engine = Engine;
        let handle = engine.start(TcpListenerFactory, handler);
        let events_rx = handle.events_tx.subscribe();
        (handle, events_rx)
    }

    /// Helper: wait for a specific event, with a timeout.
    async fn wait_for_event(
        rx: &mut broadcast::Receiver<EngineEvent>,
        matches: impl Fn(&EngineEvent) -> bool,
    ) -> EngineEvent {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match rx.recv().await {
                    Ok(ev) if matches(&ev) => return ev,
                    Ok(_) => continue,
                    Err(e) => panic!("event channel error: {e}"),
                }
            }
        })
        .await
        .expect("timed out waiting for event")
    }

    #[tokio::test]
    async fn when_start_accepting_expect_accepting_event() {
        let handler = Arc::new(MockHandler::new());
        let (handle, mut events_rx) = start_engine(handler);

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
        let handler = Arc::new(MockHandler::new());
        let (handle, mut events_rx) = start_engine(handler);

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
    async fn when_client_connects_expect_handler_called() {
        let handler = Arc::new(MockHandler::new());
        let (handle, mut events_rx) = start_engine(Arc::clone(&handler));

        handle
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "127.0.0.1:0".into(),
            })
            .await
            .unwrap();

        let addr = match wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::Accepting { .. })
        })
        .await
        {
            EngineEvent::Accepting { addr } => addr,
            _ => unreachable!(),
        };

        let _client = TcpStream::connect(&addr).await.unwrap();

        wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::ConnectionAccepted { .. })
        })
        .await;

        // Give the spawned handler task a moment to run.
        tokio::time::timeout(Duration::from_secs(2), handler.wait_for_count(1))
            .await
            .expect("handler was not called in time");

        assert_eq!(handler.count(), 1);

        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_multiple_clients_connect_expect_handler_called_for_each() {
        let handler = Arc::new(MockHandler::new());
        let (handle, mut events_rx) = start_engine(Arc::clone(&handler));

        handle
            .cmd_tx
            .send(EngineCmd::StartAccepting {
                addr: "127.0.0.1:0".into(),
            })
            .await
            .unwrap();

        let addr = match wait_for_event(&mut events_rx, |e| {
            matches!(e, EngineEvent::Accepting { .. })
        })
        .await
        {
            EngineEvent::Accepting { addr } => addr,
            _ => unreachable!(),
        };

        let _c1 = TcpStream::connect(&addr).await.unwrap();
        let _c2 = TcpStream::connect(&addr).await.unwrap();
        let _c3 = TcpStream::connect(&addr).await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), handler.wait_for_count(3))
            .await
            .expect("handler was not called 3 times in time");

        assert_eq!(handler.count(), 3);

        handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    }

    #[tokio::test]
    async fn when_bind_fails_expect_error_event() {
        let handler = Arc::new(MockHandler::new());
        let (handle, mut events_rx) = start_engine(handler);

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
}
