use anyhow::Context;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::TokioAsyncReadCompatExt;
use tracing::{debug, info, warn};

use waterdrop_core::transport::{Connection, Connector, DataStream, Listener, ListenerFactory};

/// A yamux sub-stream used for bulk data transfer over TCP.
///
/// Wraps a single [`yamux::Stream`] and implements [`DataStream`] so the
/// protocol layer can read/write without knowing the underlying transport.
pub struct TcpDataStream {
    stream: yamux::Stream,
}

impl DataStream for TcpDataStream {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = anyhow::Result<usize>> + Send + 'a {
        async move {
            self.stream
                .read(buf)
                .await
                .context("failed to read from TCP data stream")
        }
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            self.stream
                .write_all(buf)
                .await
                .context("failed to write to TCP data stream")
        }
    }

    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            self.stream
                .close()
                .await
                .context("failed to close TCP data stream")
        }
    }
}

/// A multiplexed TCP connection backed by yamux.
///
/// The first yamux stream is the **control channel** (protocol frames).
/// The control stream (first yamux sub-stream) is opened eagerly during
/// connection creation — by [`connect_yamux`] on the client side and by
/// [`TcpListener::accept`] on the server side — so the connection is
/// immediately ready for I/O when handed to a [`Session`].
///
/// Additional data streams are opened/accepted on demand for bulk file
/// transfer via [`open_stream`](Connection::open_stream) and
/// [`accept_stream`](Connection::accept_stream).
///
/// A background driver task continuously polls the [`yamux::Connection`]
/// so that all streams can make progress concurrently.
pub struct TcpConnection {
    control: yamux::Stream,
    incoming_rx: mpsc::Receiver<yamux::Stream>,
    outbound_tx: mpsc::Sender<oneshot::Sender<anyhow::Result<yamux::Stream>>>,
    peer_addr: String,
}

impl Connection for TcpConnection {
    type DataStream = TcpDataStream;

    fn peer(&self) -> String {
        self.peer_addr.clone()
    }

    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = anyhow::Result<usize>> + Send + 'a {
        async move {
            self.control
                .read(buf)
                .await
                .context("failed to read from TCP control stream")
        }
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            self.control
                .write_all(buf)
                .await
                .context("failed to write to TCP control stream")
        }
    }

    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            self.control
                .close()
                .await
                .context("failed to close TCP control stream")
        }
    }

    fn open_stream(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<Self::DataStream>> + Send + '_ {
        async move {
            let (tx, rx) = oneshot::channel();
            self.outbound_tx
                .send(tx)
                .await
                .map_err(|_| anyhow::anyhow!("yamux driver closed"))?;
            let stream = rx
                .await
                .context("yamux driver dropped the outbound request")??;
            Ok(TcpDataStream { stream })
        }
    }

    fn accept_stream(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<Self::DataStream>> + Send + '_ {
        async move {
            let stream = self
                .incoming_rx
                .recv()
                .await
                .context("yamux connection closed")?;
            Ok(TcpDataStream { stream })
        }
    }
}

/// Drives the [`yamux::Connection`] in the background.
///
/// This task must run for the lifetime of the connection — it processes
/// incoming frames from the socket and dispatches them to the correct
/// yamux sub-stream.  It also fulfils outbound-stream open requests from
/// the [`TcpConnection`].
async fn drive_yamux<T>(
    mut conn: yamux::Connection<T>,
    incoming_tx: mpsc::Sender<yamux::Stream>,
    mut outbound_rx: mpsc::Receiver<oneshot::Sender<anyhow::Result<yamux::Stream>>>,
) where
    T: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + Send,
{
    let mut pending_outbound: Option<oneshot::Sender<anyhow::Result<yamux::Stream>>> = None;

    std::future::poll_fn(|cx| {
        if let Some(ref reply) = pending_outbound {
            if reply.is_closed() {
                pending_outbound = None;
            } else if let std::task::Poll::Ready(result) = conn.poll_new_outbound(cx) {
                let reply = pending_outbound.take().expect("checked above");
                let _ = reply.send(result.map_err(|e| anyhow::anyhow!(e)));
            }
        }

        if pending_outbound.is_none()
            && let std::task::Poll::Ready(Some(reply)) = outbound_rx.poll_recv(cx)
        {
            if let std::task::Poll::Ready(result) = conn.poll_new_outbound(cx) {
                let _ = reply.send(result.map_err(|e| anyhow::anyhow!(e)));
            } else {
                pending_outbound = Some(reply);
            }
        }

        loop {
            match conn.poll_next_inbound(cx) {
                std::task::Poll::Ready(Some(Ok(stream))) => {
                    if incoming_tx.try_send(stream).is_err() {
                        return std::task::Poll::Ready(());
                    }
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    warn!(error = %e, "yamux inbound error");
                    return std::task::Poll::Ready(());
                }
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(()),
                std::task::Poll::Pending => break,
            }
        }

        std::task::Poll::Pending
    })
    .await;
}

/// A TCP listener wrapping a [`tokio::net::TcpListener`].
///
/// Every accepted connection is automatically wrapped in a yamux session
/// so that the [`TcpConnection`] supports stream multiplexing.
pub struct TcpListener {
    inner: tokio::net::TcpListener,
    local_addr: String,
}

impl Listener for TcpListener {
    type Conn = TcpConnection;

    fn local_addr(&self) -> String {
        self.local_addr.clone()
    }

    fn accept(&mut self) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send + '_ {
        async move {
            let (stream, addr) = self
                .inner
                .accept()
                .await
                .context("failed to accept TCP connection")?;

            let peer_addr = addr.to_string();
            debug!(peer = %peer_addr, "Accepted TCP connection");

            let compat = stream.compat();
            let yamux_conn =
                yamux::Connection::new(compat, yamux::Config::default(), yamux::Mode::Server);

            let (incoming_tx, incoming_rx) = mpsc::channel(16);
            let (outbound_tx, outbound_rx) = mpsc::channel(8);

            tokio::spawn(drive_yamux(yamux_conn, incoming_tx, outbound_rx));

            // Wait for the client to open the first yamux stream (control channel).
            let mut incoming_rx = incoming_rx;
            let control = incoming_rx
                .recv()
                .await
                .context("yamux connection closed before control stream was established")?;

            Ok(TcpConnection {
                control,
                incoming_rx,
                outbound_tx,
                peer_addr,
            })
        }
    }
}

/// Factory that binds [`TcpListener`] instances on the given address.
pub struct TcpListenerFactory;

impl ListenerFactory for TcpListenerFactory {
    type L = TcpListener;

    fn bind<'a>(
        &'a self,
        addr: &'a str,
    ) -> impl Future<Output = anyhow::Result<Self::L>> + Send + 'a {
        async move {
            let inner = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("failed to bind TCP listener on {addr}"))?;
            let local_addr = inner
                .local_addr()
                .context("failed to retrieve local address")?
                .to_string();
            info!(addr = %local_addr, "TCP listener bound");
            Ok(TcpListener { inner, local_addr })
        }
    }
}

// ── Outbound (client) connections ───────────────────────────────────

/// Creates a yamux-wrapped TCP client connection.
///
/// Returns a [`TcpConnection`] with the control stream already open,
/// ready for immediate I/O via the [`Connection`] trait.
///
/// # Errors
///
/// Returns an error if the TCP connection to `addr` cannot be established
/// or if the peer address cannot be retrieved from the socket.
pub async fn connect_yamux(addr: &str) -> anyhow::Result<TcpConnection> {
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .with_context(|| format!("failed to connect to {addr}"))?;

    let peer_addr = stream
        .peer_addr()
        .context("failed to get peer address")?
        .to_string();

    let compat = stream.compat();
    let yamux_conn = yamux::Connection::new(compat, yamux::Config::default(), yamux::Mode::Client);

    let (incoming_tx, incoming_rx) = mpsc::channel(16);
    let (outbound_tx, outbound_rx) = mpsc::channel(8);

    tokio::spawn(drive_yamux(yamux_conn, incoming_tx, outbound_rx));

    // Open the first yamux stream (control channel).
    let (tx, rx) = oneshot::channel();
    outbound_tx
        .send(tx)
        .await
        .map_err(|_| anyhow::anyhow!("yamux driver closed"))?;
    let control = rx
        .await
        .context("yamux driver dropped the outbound request")??;

    Ok(TcpConnection {
        control,
        incoming_rx,
        outbound_tx,
        peer_addr,
    })
}

/// Factory that creates outbound [`TcpConnection`]s (client side).
///
/// This is the client-mode counterpart to [`TcpListenerFactory`].
pub struct TcpConnector;

impl Connector for TcpConnector {
    type Conn = TcpConnection;

    fn connect<'a>(
        &'a self,
        addr: &'a str,
    ) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send + 'a {
        connect_yamux(addr)
    }
}

#[cfg(test)]
mod tests {
    use waterdrop_core::transport::DataStream as _;

    use super::*;

    #[tokio::test]
    async fn given_invalid_address_when_binding_then_returns_error() {
        let factory = TcpListenerFactory;
        let result = factory.bind("999.999.999.999:0").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_valid_address_when_binding_then_returns_listener_with_local_addr() {
        let factory = TcpListenerFactory;
        let listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();
        assert!(addr.starts_with("127.0.0.1:"));
        let port: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();
        assert_ne!(port, 0);
    }

    #[tokio::test]
    async fn given_client_connects_when_accepted_then_peer_is_loopback() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();
            // Open the control stream from client side so the server
            // can accept the connection successfully.
            client.write_all(b"hi").await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let conn = listener.accept().await.unwrap();
        let peer: std::net::SocketAddr = conn.peer().parse().unwrap();
        assert!(peer.ip().is_loopback());

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_two_clients_when_accepted_then_peers_are_distinct() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let a = addr.clone();
        let h1 = tokio::spawn(async move {
            let mut c = connect_yamux(&a).await.unwrap();
            c.write_all(b"a").await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        });

        let h2 = tokio::spawn(async move {
            let mut c = connect_yamux(&addr).await.unwrap();
            c.write_all(b"b").await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        });

        let conn1 = listener.accept().await.unwrap();
        let conn2 = listener.accept().await.unwrap();
        assert_ne!(conn1.peer(), conn2.peer());

        h1.await.unwrap();
        h2.await.unwrap();
    }

    #[tokio::test]
    async fn given_client_sends_data_when_server_reads_then_data_matches() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();
            client.write_all(b"hello from client").await.unwrap();
            client.shutdown().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from client");

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_server_sends_data_when_client_reads_then_data_matches() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();

            // Client writes first to trigger control stream.
            client.write_all(b"greeting").await.unwrap();

            let mut buf = [0u8; 64];
            let n = client.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"greeting");

        server_conn.write_all(b"hello from server").await.unwrap();
        server_conn.shutdown().await.unwrap();

        let client_received = client_handle.await.unwrap();
        assert_eq!(client_received, "hello from server");
    }

    /// Yamux requires at least one data frame before close to send the SYN
    /// flag.  This test writes a single byte, shuts down, and verifies the
    /// server reads the byte followed by EOF (`0`).
    #[tokio::test]
    async fn given_client_writes_then_closes_when_server_reads_then_sees_data_and_eof() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();
            client.write_all(b"x").await.unwrap();
            client.shutdown().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];

        // Read the data byte.
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"x");

        // Next read should return 0 (EOF).
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_client_opens_data_stream_when_server_accepts_then_data_flows() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();

            // Control stream.
            client.write_all(b"ctrl").await.unwrap();

            // Data stream.
            let mut data = client.open_stream().await.unwrap();
            data.write_all(b"file-bytes-123").await.unwrap();
            data.shutdown().await.unwrap();

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ctrl");

        let mut data_stream = server_conn.accept_stream().await.unwrap();
        let mut data_buf = [0u8; 64];
        let n = data_stream.read(&mut data_buf).await.unwrap();
        assert_eq!(&data_buf[..n], b"file-bytes-123");

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_server_opens_data_stream_when_client_accepts_then_data_flows() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();

            // Open control stream from client.
            client.write_all(b"ctrl").await.unwrap();

            // Accept data stream opened by server.
            let mut data = client.accept_stream().await.unwrap();
            let mut buf = [0u8; 64];
            let n = data.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ctrl");

        let mut data_stream = server_conn.open_stream().await.unwrap();
        data_stream.write_all(b"server-payload").await.unwrap();
        data_stream.shutdown().await.unwrap();

        let client_received = client_handle.await.unwrap();
        assert_eq!(client_received, "server-payload");
    }

    #[tokio::test]
    async fn given_large_payload_on_data_stream_when_received_then_data_is_intact() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let payload: Vec<u8> = (0u32..131_072).map(|i| (i % 251) as u8).collect();
        let payload_clone = payload.clone();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();

            // Control stream.
            client.write_all(b"go").await.unwrap();

            // Data stream with large payload.
            let mut data = client.open_stream().await.unwrap();
            data.write_all(&payload_clone).await.unwrap();
            data.shutdown().await.unwrap();

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let _n = server_conn.read(&mut buf).await.unwrap();

        let mut data_stream = server_conn.accept_stream().await.unwrap();
        let mut received = Vec::new();
        let mut data_buf = [0u8; 4096];
        loop {
            let n = data_stream.read(&mut data_buf).await.unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&data_buf[..n]);
        }

        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);

        client_handle.await.unwrap();
    }

    /// Yamux requires at least one data frame before close to send the SYN
    /// flag.  This test writes a single marker byte on the data stream,
    /// shuts down, and verifies the server reads the marker followed by EOF.
    #[tokio::test]
    async fn given_data_stream_written_then_closed_when_read_then_sees_data_and_eof() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();

            // Control stream.
            client.write_all(b"hi").await.unwrap();

            // Data stream — write a marker then close.
            let mut data = client.open_stream().await.unwrap();
            data.write_all(b"z").await.unwrap();
            data.shutdown().await.unwrap();

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let _n = server_conn.read(&mut buf).await.unwrap();

        let mut data_stream = server_conn.accept_stream().await.unwrap();
        let mut data_buf = [0u8; 64];

        // Read the marker byte.
        let n = data_stream.read(&mut data_buf).await.unwrap();
        assert_eq!(&data_buf[..n], b"z");

        // Next read should return 0 (EOF).
        let n = data_stream.read(&mut data_buf).await.unwrap();
        assert_eq!(n, 0);

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_bidirectional_exchange_when_both_sides_send_then_both_receive() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut client = connect_yamux(&addr).await.unwrap();

            client.write_all(b"ping").await.unwrap();
            client.shutdown().await.unwrap();

            // Accept response on data stream.
            let mut data = client.accept_stream().await.unwrap();
            let mut buf = [0u8; 64];
            let n = data.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");

        // Reply on a data stream.
        let mut data = server_conn.open_stream().await.unwrap();
        data.write_all(b"pong").await.unwrap();
        data.shutdown().await.unwrap();

        let client_received = client_handle.await.unwrap();
        assert_eq!(client_received, "pong");
    }

    // ---------------------------------------------------------------
    //  TCP + Session functional integration tests
    //
    //  These exercise real TCP/yamux transport through the full Session
    //  state machine to ensure the control-stream drain logic works
    //  correctly and no data is lost at the end of a transfer.
    // ---------------------------------------------------------------

    use std::time::Duration;
    use tokio::sync::mpsc;

    use crate::session::{Role, SendRequest, Session, SessionCmd, SessionEvent, SessionHandle};

    /// Collects session events until `pred` returns true or `timeout` expires.
    async fn collect_session_events_until(
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

    /// Waits for a specific session event, panicking on timeout.
    async fn wait_for_session_event(
        rx: &mut mpsc::Receiver<SessionEvent>,
        pred: impl Fn(&SessionEvent) -> bool,
    ) -> SessionEvent {
        let events = collect_session_events_until(rx, &pred, Duration::from_secs(10)).await;
        events
            .into_iter()
            .rfind(|e| pred(e))
            .expect("timed out waiting for session event")
    }

    /// Helper: create a TCP/yamux listener + client connection pair for
    /// session tests.
    async fn tcp_session_pair(
        send_dir: &std::path::Path,
        recv_dir: &std::path::Path,
    ) -> (SessionHandle, SessionHandle) {
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

    /// Given a real TCP connection, when a file is transferred and
    /// accepted, then both sides emit TransferComplete and Finished
    /// without any errors.
    #[tokio::test]
    async fn given_tcp_transfer_when_accepted_then_both_sides_complete_without_error() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        let content = b"TCP integration test payload - clean finish";
        tokio::fs::write(send_dir.path().join("send_file.txt"), content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) = tcp_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "tcp-xfer-1".into(),
            file_path: send_dir.path().join("send_file.txt"),
            filename: "received.txt".into(),
            size_bytes: content.len() as u64,
            sha256_hex: "n/a".into(),
        };
        handle_c
            .cmd_tx
            .send(SessionCmd::Transfer { req })
            .await
            .unwrap();

        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;
        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: "tcp-xfer-1".into(),
                accept: true,
            })
            .await
            .unwrap();

        // Client: collect all events until Finished.
        let client_events = collect_session_events_until(
            &mut handle_c.event_rx,
            |e| matches!(e, SessionEvent::Finished),
            Duration::from_secs(10),
        )
        .await;

        assert!(
            client_events
                .iter()
                .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
            "client must see TransferComplete, got: {client_events:?}"
        );
        assert!(
            client_events
                .iter()
                .any(|e| matches!(e, SessionEvent::Finished)),
            "client must see Finished, got: {client_events:?}"
        );
        assert!(
            !client_events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "client must NOT see Error, got: {client_events:?}"
        );

        // Server: collect all events until Finished.
        let server_events = collect_session_events_until(
            &mut handle_s.event_rx,
            |e| matches!(e, SessionEvent::Finished),
            Duration::from_secs(10),
        )
        .await;

        assert!(
            server_events
                .iter()
                .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
            "server must see TransferComplete, got: {server_events:?}"
        );
        assert!(
            !server_events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "server must NOT see Error, got: {server_events:?}"
        );

        // Verify file content.
        let received = tokio::fs::read(recv_dir.path().join("received.txt"))
            .await
            .unwrap();
        assert_eq!(received, content);
    }

    /// Given a real TCP connection, when a large file (256 KB) is
    /// transferred, then progress events are emitted and data arrives
    /// intact.
    #[tokio::test]
    async fn given_tcp_large_file_when_transferred_then_progress_emitted_and_data_intact() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        #[allow(clippy::cast_possible_truncation)]
        let content: Vec<u8> = (0..262_144u32).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(send_dir.path().join("send_file.txt"), &content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) = tcp_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "tcp-big".into(),
            file_path: send_dir.path().join("send_file.txt"),
            filename: "big.bin".into(),
            size_bytes: content.len() as u64,
            sha256_hex: "n/a".into(),
        };
        handle_c
            .cmd_tx
            .send(SessionCmd::Transfer { req })
            .await
            .unwrap();

        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;
        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: "tcp-big".into(),
                accept: true,
            })
            .await
            .unwrap();

        // Client events.
        let c_events = collect_session_events_until(
            &mut handle_c.event_rx,
            |e| matches!(e, SessionEvent::TransferComplete { .. }),
            Duration::from_secs(15),
        )
        .await;

        let c_progress = c_events
            .iter()
            .filter(|e| matches!(e, SessionEvent::TransferProgress { .. }))
            .count();
        assert!(
            c_progress >= 2,
            "expected >= 2 client progress events, got {c_progress}"
        );
        assert!(
            !c_events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "client must NOT see Error, got: {c_events:?}"
        );

        // Server events.
        let s_events = collect_session_events_until(
            &mut handle_s.event_rx,
            |e| matches!(e, SessionEvent::TransferComplete { .. }),
            Duration::from_secs(15),
        )
        .await;

        let s_progress = s_events
            .iter()
            .filter(|e| matches!(e, SessionEvent::TransferProgress { .. }))
            .count();
        assert!(
            s_progress >= 2,
            "expected >= 2 server progress events, got {s_progress}"
        );

        // Verify data integrity.
        let received = tokio::fs::read(recv_dir.path().join("big.bin"))
            .await
            .unwrap();
        assert_eq!(received.len(), content.len());
        assert_eq!(received, content);
    }

    /// Given a real TCP connection, when the server denies the transfer,
    /// then the client sees TransferDenied and both sides remain
    /// operational.
    #[tokio::test]
    async fn given_tcp_transfer_when_denied_then_client_sees_denied_without_error() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        let content = b"deny-me over TCP";
        tokio::fs::write(send_dir.path().join("send_file.txt"), content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) = tcp_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "tcp-deny".into(),
            file_path: send_dir.path().join("send_file.txt"),
            filename: "nope.txt".into(),
            size_bytes: content.len() as u64,
            sha256_hex: "n/a".into(),
        };
        handle_c
            .cmd_tx
            .send(SessionCmd::Transfer { req })
            .await
            .unwrap();

        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;

        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: "tcp-deny".into(),
                accept: false,
            })
            .await
            .unwrap();

        let ev = wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::TransferDenied { .. })
        })
        .await;
        assert!(
            matches!(ev, SessionEvent::TransferDenied { transfer_id } if transfer_id == "tcp-deny")
        );

        // Clean shutdown.
        let _ = handle_c.cmd_tx.send(SessionCmd::Cancel).await;
        let _ = handle_s.cmd_tx.send(SessionCmd::Cancel).await;
    }

    /// Given a real TCP connection, when the handshake completes, then
    /// both sides report the correct peer device name.
    #[tokio::test]
    async fn given_tcp_connection_when_handshake_completes_then_peer_names_are_correct() {
        let dir = tempfile::tempdir().unwrap();

        let (mut handle_c, mut handle_s) = tcp_session_pair(dir.path(), dir.path()).await;

        let ev_c = wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        let SessionEvent::Connected {
            peer_device_name: client_sees,
        } = ev_c
        else {
            unreachable!()
        };
        assert_eq!(client_sees, "TcpServer");

        let ev_s = wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        let SessionEvent::Connected {
            peer_device_name: server_sees,
        } = ev_s
        else {
            unreachable!()
        };
        assert_eq!(server_sees, "TcpClient");

        let _ = handle_c.cmd_tx.send(SessionCmd::Cancel).await;
        let _ = handle_s.cmd_tx.send(SessionCmd::Cancel).await;
    }

    /// Given a real TCP connection, when the client cancels mid-session,
    /// then both sides terminate without panicking.
    #[tokio::test]
    async fn given_tcp_session_when_client_cancels_then_both_sides_terminate() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        let content = b"cancel me over TCP";
        tokio::fs::write(send_dir.path().join("send_file.txt"), content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) = tcp_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "tcp-cancel".into(),
            file_path: send_dir.path().join("send_file.txt"),
            filename: "x.txt".into(),
            size_bytes: content.len() as u64,
            sha256_hex: "n/a".into(),
        };
        handle_c
            .cmd_tx
            .send(SessionCmd::Transfer { req })
            .await
            .unwrap();

        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;

        // Cancel client before server responds.
        handle_c.cmd_tx.send(SessionCmd::Cancel).await.unwrap();

        let ev_c = wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Finished)
        })
        .await;
        assert!(matches!(ev_c, SessionEvent::Finished));

        // Server should also terminate.
        let ev_s = wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Finished | SessionEvent::Error { .. })
        })
        .await;
        assert!(matches!(
            ev_s,
            SessionEvent::Finished | SessionEvent::Error { .. }
        ));
    }

    /// Given a real TCP connection with an empty file, when transferred,
    /// then both sides complete and the empty file is created.
    #[tokio::test]
    async fn given_tcp_empty_file_when_transferred_then_both_sides_complete() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        tokio::fs::write(send_dir.path().join("send_file.txt"), b"")
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) = tcp_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "tcp-empty".into(),
            file_path: send_dir.path().join("send_file.txt"),
            filename: "empty.txt".into(),
            size_bytes: 0,
            sha256_hex: "n/a".into(),
        };
        handle_c
            .cmd_tx
            .send(SessionCmd::Transfer { req })
            .await
            .unwrap();

        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;
        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: "tcp-empty".into(),
                accept: true,
            })
            .await
            .unwrap();

        // Both complete without error.
        let c_events = collect_session_events_until(
            &mut handle_c.event_rx,
            |e| matches!(e, SessionEvent::Finished),
            Duration::from_secs(10),
        )
        .await;
        assert!(
            c_events
                .iter()
                .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
            "client must see TransferComplete"
        );
        assert!(
            !c_events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "client must NOT see Error, got: {c_events:?}"
        );

        let s_events = collect_session_events_until(
            &mut handle_s.event_rx,
            |e| matches!(e, SessionEvent::Finished),
            Duration::from_secs(10),
        )
        .await;
        assert!(
            s_events
                .iter()
                .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
            "server must see TransferComplete"
        );

        let received = tokio::fs::read(recv_dir.path().join("empty.txt"))
            .await
            .unwrap();
        assert!(received.is_empty());
    }
}
