//! Shared helpers for `waterdrop-engine` integration tests.
//!
//! Provides:
//! - [`MemoryPipe`] / [`MockConnection`]: in-memory transport for fast,
//!   deterministic session tests.
//! - Event-collection utilities ([`collect_events_until`], [`wait_for_event`]).
//! - Session-pair factories for each transport
//!   ([`mock_session_pair`], [`quic_session_pair`], [`tcp_session_pair`]).

#![allow(unused_imports, dead_code)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

use waterdrop_core::transport::{Connection, DataStream, Listener, ListenerFactory};
use waterdrop_engine::quic::{QuicConnector, QuicListenerFactory, connect_quic};
use waterdrop_engine::session::{Role, SendRequest, Session, SessionEvent, SessionHandle};
use waterdrop_engine::tcp::{TcpListenerFactory, connect_yamux};

/// A bidirectional in-memory byte pipe implementing [`DataStream`].
///
/// Created in pairs via [`MemoryPipe::pair`]; what one side writes, the
/// other side reads.
pub struct MemoryPipe {
    read_buf: Arc<Mutex<VecDeque<u8>>>,
    write_buf: Arc<Mutex<VecDeque<u8>>>,
    read_notify: Arc<tokio::sync::Notify>,
    write_notify: Arc<tokio::sync::Notify>,
    closed: Arc<Mutex<bool>>,
}

impl MemoryPipe {
    /// Creates a connected pair of pipes.
    pub fn pair() -> (Self, Self) {
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

/// A [`Connection`] backed entirely by in-memory pipes.
///
/// Created in pairs via [`MockConnection::pair`].  Suitable for fast
/// session-level tests that do not need a real network.
pub struct MockConnection {
    control: MemoryPipe,
    outgoing_streams: Arc<Mutex<Vec<MemoryPipe>>>,
    incoming_streams: Arc<Mutex<Vec<MemoryPipe>>>,
    stream_notify: Arc<tokio::sync::Notify>,
    peer_name: String,
}

impl MockConnection {
    /// Creates a pair of connected mock connections.
    pub fn pair(name_a: &str, name_b: &str) -> (Self, Self) {
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

/// Collects [`SessionEvent`]s until `pred` returns `true` or `timeout`
/// expires, whichever comes first.
pub async fn collect_events_until(
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

/// Waits for a single [`SessionEvent`] matching `pred`, panicking if
/// none arrives within 10 seconds.
pub async fn wait_for_event(
    rx: &mut mpsc::Receiver<SessionEvent>,
    pred: impl Fn(&SessionEvent) -> bool,
) -> SessionEvent {
    let events = collect_events_until(rx, &pred, Duration::from_secs(10)).await;
    events
        .into_iter()
        .rfind(|e| pred(e))
        .expect("timed out waiting for session event")
}

/// Builds a [`SendRequest`] with sensible defaults for tests.
pub fn make_send_request(
    transfer_id: &str,
    send_dir: &Path,
    filename: &str,
    size_bytes: u64,
) -> SendRequest {
    SendRequest {
        transfer_id: transfer_id.into(),
        file_path: send_dir.join("send_file.txt"),
        filename: filename.into(),
        size_bytes,
        sha256_hex: "n/a".into(),
    }
}

/// Creates a client/server [`SessionHandle`] pair over in-memory
/// [`MockConnection`]s.
pub fn mock_session_pair(send_dir: &Path, recv_dir: &Path) -> (SessionHandle, SessionHandle) {
    let (conn_c, conn_s) = MockConnection::pair("sender", "receiver");

    let handle_s = Session::spawn(
        conn_s,
        Role::Server,
        "MockServer".into(),
        recv_dir.to_path_buf(),
    );
    let handle_c = Session::spawn(
        conn_c,
        Role::Client,
        "MockClient".into(),
        send_dir.to_path_buf(),
    );

    (handle_c, handle_s)
}

/// Creates a client/server [`SessionHandle`] pair over real QUIC
/// (loopback).
///
/// The server accept and client connect happen concurrently to avoid
/// deadlocking the single-threaded test runtime.
pub async fn quic_session_pair(send_dir: &Path, recv_dir: &Path) -> (SessionHandle, SessionHandle) {
    let cert_dir = tempfile::tempdir().unwrap();
    let trust_dir = tempfile::tempdir().unwrap();

    let factory = QuicListenerFactory::new(cert_dir.path()).unwrap();
    let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr();

    let recv_path = recv_dir.to_path_buf();
    let server_task = tokio::spawn(async move {
        let conn = listener.accept().await.unwrap();
        Session::spawn(conn, Role::Server, "QuicServer".into(), recv_path)
    });

    let trust_path = trust_dir.path().to_path_buf();
    let client_conn = connect_quic(&addr, &trust_path).await.unwrap();
    let handle_c = Session::spawn(
        client_conn,
        Role::Client,
        "QuicClient".into(),
        send_dir.to_path_buf(),
    );

    let handle_s = server_task.await.unwrap();

    // Leak the tempdirs so they live long enough for the sessions.
    std::mem::forget(cert_dir);
    std::mem::forget(trust_dir);

    (handle_c, handle_s)
}

/// Creates a client/server [`SessionHandle`] pair over real TCP/yamux
/// (loopback).
pub async fn tcp_session_pair(send_dir: &Path, recv_dir: &Path) -> (SessionHandle, SessionHandle) {
    let factory = TcpListenerFactory;
    let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr();

    let recv_path = recv_dir.to_path_buf();
    let server_task = tokio::spawn(async move {
        let conn = listener.accept().await.unwrap();
        Session::spawn(conn, Role::Server, "TcpServer".into(), recv_path)
    });

    let client_conn = connect_yamux(&addr).await.unwrap();
    let handle_c = Session::spawn(
        client_conn,
        Role::Client,
        "TcpClient".into(),
        send_dir.to_path_buf(),
    );

    let handle_s = server_task.await.unwrap();
    (handle_c, handle_s)
}
