use std::sync::Arc;

use anyhow::Context;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use tracing::{debug, info};

use waterdrop_core::tls;
use waterdrop_core::transport::{Connection, Connector, DataStream, Listener, ListenerFactory};

const ALPN_PROTOCOL: &[u8] = b"waterdrop/1";

/// A bidirectional QUIC sub-stream used for bulk data transfer.
///
/// Wraps a pair of [`quinn::SendStream`] and [`quinn::RecvStream`] opened
/// via [`QuicConnection::open_stream`] or [`QuicConnection::accept_stream`].
pub struct QuicDataStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl DataStream for QuicDataStream {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = anyhow::Result<usize>> + Send + 'a {
        async move {
            self.recv
                .read(buf)
                .await
                .context("failed to read from QUIC data stream")?
                .map_or(Ok(0), Ok)
        }
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            self.send
                .write_all(buf)
                .await
                .context("failed to write to QUIC data stream")
        }
    }

    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            self.send
                .finish()
                .context("failed to finish QUIC data stream")
        }
    }
}

/// A QUIC connection with an eagerly-established control stream.
///
/// The control stream (first bidirectional QUIC stream) is opened during
/// connection creation — by [`connect_quic`] on the client side and by
/// [`QuicListener::accept`] on the server side — so the connection is
/// immediately ready for I/O when handed to a [`Session`].
///
/// Additional data streams can be opened via [`open_stream`](Connection::open_stream)
/// or accepted via [`accept_stream`](Connection::accept_stream).  These are
/// independent of the control channel and can be shut down or dropped
/// without affecting control traffic.
pub struct QuicConnection {
    connection: quinn::Connection,
    /// Keeps the client-side endpoint alive for the lifetime of the
    /// connection.  Server-side connections set this to `None` because
    /// the [`QuicListener`] already owns the endpoint.
    _endpoint: Option<quinn::Endpoint>,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    peer_addr: String,
}

impl Connection for QuicConnection {
    type DataStream = QuicDataStream;

    fn peer(&self) -> String {
        self.peer_addr.clone()
    }

    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = anyhow::Result<usize>> + Send + 'a {
        async move {
            self.recv
                .read(buf)
                .await
                .context("failed to read from QUIC stream")?
                .map_or(Ok(0), Ok)
        }
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            self.send
                .write_all(buf)
                .await
                .context("failed to write to QUIC stream")
        }
    }

    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            self.send
                .finish()
                .context("failed to finish QUIC send stream")
        }
    }

    fn open_stream(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<Self::DataStream>> + Send + '_ {
        async move {
            let (send, recv) = self
                .connection
                .open_bi()
                .await
                .context("failed to open QUIC data stream")?;
            Ok(QuicDataStream { send, recv })
        }
    }

    fn accept_stream(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<Self::DataStream>> + Send + '_ {
        async move {
            let (send, recv) = self
                .connection
                .accept_bi()
                .await
                .context("failed to accept QUIC data stream")?;
            Ok(QuicDataStream { send, recv })
        }
    }
}

/// A QUIC listener wrapping a [`quinn::Endpoint`].
pub struct QuicListener {
    endpoint: quinn::Endpoint,
    local_addr: String,
}

impl Listener for QuicListener {
    type Conn = QuicConnection;

    fn local_addr(&self) -> String {
        self.local_addr.clone()
    }

    fn accept(&mut self) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send + '_ {
        async move {
            let incoming = self
                .endpoint
                .accept()
                .await
                .context("QUIC endpoint closed")?;

            let connection = incoming
                .await
                .context("failed to complete QUIC handshake")?;

            let peer_addr = connection.remote_address().to_string();
            debug!(peer = %peer_addr, "Accepted QUIC connection");

            let (send, recv) = connection
                .accept_bi()
                .await
                .context("failed to accept control stream from client")?;

            Ok(QuicConnection {
                connection,
                _endpoint: None,
                send,
                recv,
                peer_addr,
            })
        }
    }
}

/// Factory that binds [`QuicListener`] instances on the given address.
///
/// A self-signed certificate is generated at construction time and reused
/// for every listener.  Authentication happens at the WaterDrop protocol
/// layer (Ed25519 signatures), not at TLS level.
pub struct QuicListenerFactory {
    server_config: quinn::ServerConfig,
}

impl QuicListenerFactory {
    /// # Errors
    ///
    /// Returns an error if certificate generation or TLS configuration fails.
    pub fn new() -> anyhow::Result<Self> {
        let server_config = build_server_config()?;
        Ok(Self { server_config })
    }
}

impl ListenerFactory for QuicListenerFactory {
    type L = QuicListener;

    fn bind<'a>(
        &'a self,
        addr: &'a str,
    ) -> impl Future<Output = anyhow::Result<Self::L>> + Send + 'a {
        async move {
            let socket_addr: std::net::SocketAddr = addr
                .parse()
                .with_context(|| format!("invalid bind address: {addr}"))?;

            let endpoint = quinn::Endpoint::server(self.server_config.clone(), socket_addr)
                .with_context(|| format!("failed to bind QUIC endpoint on {addr}"))?;

            let local_addr = endpoint
                .local_addr()
                .context("failed to retrieve local address")?
                .to_string();

            info!(addr = %local_addr, "QUIC listener bound");

            Ok(QuicListener {
                endpoint,
                local_addr,
            })
        }
    }
}

/// Creates a QUIC client connection to the given address.
///
/// Returns a [`QuicConnection`] with the control stream already open,
/// ready for immediate I/O via the [`Connection`] trait.
///
/// # Errors
///
/// Returns an error if the address cannot be parsed, the client endpoint
/// cannot be created, or the QUIC handshake fails.
pub async fn connect_quic(addr: &str) -> anyhow::Result<QuicConnection> {
    let socket_addr: std::net::SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid connect address: {addr}"))?;

    let bind_addr: std::net::SocketAddr = "0.0.0.0:0"
        .parse()
        .context("failed to parse ephemeral bind address")?;
    let mut endpoint =
        quinn::Endpoint::client(bind_addr).context("failed to create QUIC client endpoint")?;
    endpoint.set_default_client_config(insecure_client_config()?);

    let connection = endpoint
        .connect(socket_addr, "localhost")
        .with_context(|| format!("failed to start QUIC connection to {addr}"))?
        .await
        .with_context(|| format!("QUIC handshake failed with {addr}"))?;

    let peer_addr = connection.remote_address().to_string();
    debug!(peer = %peer_addr, "QUIC client connected");

    let (send, recv) = connection
        .open_bi()
        .await
        .context("failed to open control stream")?;

    Ok(QuicConnection {
        connection,
        _endpoint: Some(endpoint),
        send,
        recv,
        peer_addr,
    })
}

/// Factory that creates outbound [`QuicConnection`]s (client side).
///
/// This is the client-mode counterpart to [`QuicListenerFactory`].
/// A fresh ephemeral endpoint is created for each connection with an
/// insecure TLS configuration (server certificate verification is
/// skipped because authentication happens at the WaterDrop protocol
/// layer).
pub struct QuicConnector;

impl Connector for QuicConnector {
    type Conn = QuicConnection;

    fn connect<'a>(
        &'a self,
        addr: &'a str,
    ) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send + 'a {
        connect_quic(addr)
    }
}

fn build_server_config() -> anyhow::Result<quinn::ServerConfig> {
    let pair = tls::generate_self_signed_cert(&["localhost"])?;

    let cert_der = rustls::pki_types::CertificateDer::from(pair.cert_der);
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(pair.private_key_pkcs8_der),
    );

    let mut tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .context("failed to build rustls server config")?;

    tls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_config: QuicServerConfig = tls_config
        .try_into()
        .context("failed to build QUIC server config")?;

    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic_config)))
}

/// Builds a QUIC client config that skips server certificate verification.
///
/// This is appropriate for WaterDrop because authentication happens at the
/// protocol layer (Ed25519 signatures), not at TLS level.
///
/// # Errors
///
/// Returns an error if the TLS client configuration fails to build.
pub fn insecure_client_config() -> anyhow::Result<quinn::ClientConfig> {
    let mut tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    tls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_config: QuicClientConfig = tls_config
        .try_into()
        .context("failed to build QUIC client config")?;

    Ok(quinn::ClientConfig::new(Arc::new(quic_config)))
}

#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_client_endpoint() -> anyhow::Result<quinn::Endpoint> {
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
            .context("failed to create client endpoint")?;
        endpoint.set_default_client_config(insecure_client_config()?);
        Ok(endpoint)
    }

    #[tokio::test]
    async fn given_invalid_address_when_binding_then_returns_error() {
        let factory = QuicListenerFactory::new().unwrap();
        let result = factory.bind("999.999.999.999:0").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_valid_address_when_binding_then_returns_listener_with_local_addr() {
        let factory = QuicListenerFactory::new().unwrap();
        let listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();
        assert!(addr.starts_with("127.0.0.1:"));
        let port: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();
        assert_ne!(port, 0);
    }

    #[tokio::test]
    async fn given_client_connects_when_accepted_then_peer_address_matches() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        // Spawn the server side so that accept (which now eagerly waits
        // for the control stream) can run concurrently with connect_quic.
        let server_handle = tokio::spawn(async move { listener.accept().await.unwrap() });

        let mut client_conn = connect_quic(&addr).await.unwrap();
        let client_peer: std::net::SocketAddr = client_conn.peer().parse().unwrap();

        // Drive the control channel so the server side can complete
        // (QUIC coalesces the stream header with the first data write).
        client_conn.write_all(b"ping").await.unwrap();

        let mut server_conn = server_handle.await.unwrap();
        let server_peer: std::net::SocketAddr = server_conn.peer().parse().unwrap();

        // Drain the ping so the server connection is clean.
        let mut buf = [0u8; 16];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");

        assert_eq!(server_peer.ip(), client_peer.ip());
    }

    #[tokio::test]
    async fn given_client_sends_data_when_server_reads_then_data_matches() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let (mut send, _recv) = connection.open_bi().await.unwrap();
            send.write_all(b"hello from client").await.unwrap();
            send.finish().unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from client");

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_server_sends_data_when_client_reads_then_data_matches() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();

            // Client writes first, matching the WaterDrop protocol flow
            // where the sender always sends HELLO before the receiver responds.
            send.write_all(b"greeting").await.unwrap();

            let mut buf = [0u8; 64];
            let n = recv.read(&mut buf).await.unwrap().unwrap_or(0);
            assert_eq!(&buf[..n], b"hello from server");
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"greeting");

        server_conn.write_all(b"hello from server").await.unwrap();
        server_conn.shutdown().await.unwrap();

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_client_closes_stream_when_server_reads_then_returns_zero() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let (mut send, _recv) = connection.open_bi().await.unwrap();
            send.finish().unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);

        client_handle.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::similar_names)]
    async fn given_two_clients_when_accepted_then_peers_are_distinct() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        // Spawn the server side so that both accepts can run concurrently
        // with the client connections.
        let server_handle = tokio::spawn(async move {
            let conn1 = listener.accept().await.unwrap();
            let conn2 = listener.accept().await.unwrap();
            (conn1, conn2)
        });

        // Write data on each control channel to flush the QUIC stream
        // frames (QUIC coalesces the stream header with the first write).
        let addr1 = addr.clone();
        let mut client1 = connect_quic(&addr1).await.unwrap();
        client1.write_all(b"c1").await.unwrap();

        let mut client2 = connect_quic(&addr).await.unwrap();
        client2.write_all(b"c2").await.unwrap();

        let (conn1, conn2) = server_handle.await.unwrap();
        assert_ne!(conn1.peer(), conn2.peer());
    }

    #[tokio::test]
    async fn given_large_payload_when_sent_and_received_then_data_is_intact() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        #[allow(clippy::cast_sign_loss)]
        let payload: Vec<u8> = (0..131_072).map(|i| (i % 251) as u8).collect();
        let payload_clone = payload.clone();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let (mut send, _recv) = connection.open_bi().await.unwrap();
            send.write_all(&payload_clone).await.unwrap();
            send.finish().unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = server_conn.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&buf[..n]);
        }

        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_bidirectional_exchange_when_both_sides_send_then_both_receive() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();

            send.write_all(b"ping").await.unwrap();
            send.finish().unwrap();

            let mut buf = [0u8; 64];
            let n = recv.read(&mut buf).await.unwrap().unwrap_or(0);
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");

        server_conn.write_all(b"pong").await.unwrap();
        server_conn.shutdown().await.unwrap();

        let client_received = client_handle.await.unwrap();
        assert_eq!(client_received, "pong");
    }

    // ── Data-stream tests ───────────────────────────────────────

    #[tokio::test]
    async fn given_client_opens_data_stream_when_server_accepts_then_data_flows() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            // Open the control stream first (stream 0).
            let (mut ctrl_send, _ctrl_recv) = connection.open_bi().await.unwrap();
            ctrl_send.write_all(b"hello").await.unwrap();

            // Open a second stream for data (stream 1).
            let (mut data_send, _data_recv) = connection.open_bi().await.unwrap();
            data_send.write_all(b"file-data-abc").await.unwrap();
            data_send.finish().unwrap();

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();

        // Read from control channel.
        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");

        // Accept data stream.
        let mut data_stream = server_conn.accept_stream().await.unwrap();
        let mut data_buf = [0u8; 64];
        let n = data_stream.read(&mut data_buf).await.unwrap();
        assert_eq!(&data_buf[..n], b"file-data-abc");

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_server_opens_data_stream_when_client_accepts_then_data_flows() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            // Open the control stream (stream 0).
            let (mut ctrl_send, _ctrl_recv) = connection.open_bi().await.unwrap();
            ctrl_send.write_all(b"ctrl").await.unwrap();

            // Accept data stream opened by server.
            let (_, mut data_recv) = connection.accept_bi().await.unwrap();
            let mut buf = [0u8; 64];
            let n = data_recv.read(&mut buf).await.unwrap().unwrap_or(0);
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut server_conn = listener.accept().await.unwrap();

        // Read from control channel.
        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ctrl");

        // Server opens a data stream.
        let mut data_stream = server_conn.open_stream().await.unwrap();
        data_stream.write_all(b"server-data").await.unwrap();
        data_stream.shutdown().await.unwrap();

        let client_received = client_handle.await.unwrap();
        assert_eq!(client_received, "server-data");
    }

    #[tokio::test]
    async fn given_large_payload_on_data_stream_when_received_then_data_is_intact() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let payload: Vec<u8> = (0u32..131_072).map(|i| (i % 251) as u8).collect();
        let payload_clone = payload.clone();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            // Control stream first.
            let (mut ctrl_send, _ctrl_recv) = connection.open_bi().await.unwrap();
            ctrl_send.write_all(b"go").await.unwrap();

            // Data stream with large payload.
            let (mut data_send, _data_recv) = connection.open_bi().await.unwrap();
            data_send.write_all(&payload_clone).await.unwrap();
            data_send.finish().unwrap();

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

    #[tokio::test]
    async fn given_data_stream_closed_when_read_then_returns_zero() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();

            // Control stream.
            let (mut ctrl_send, _ctrl_recv) = connection.open_bi().await.unwrap();
            ctrl_send.write_all(b"hi").await.unwrap();

            // Data stream — finish immediately.
            let (mut data_send, _data_recv) = connection.open_bi().await.unwrap();
            data_send.finish().unwrap();

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let mut server_conn = listener.accept().await.unwrap();

        let mut buf = [0u8; 64];
        let _n = server_conn.read(&mut buf).await.unwrap();

        let mut data_stream = server_conn.accept_stream().await.unwrap();
        let mut data_buf = [0u8; 64];
        let n = data_stream.read(&mut data_buf).await.unwrap();
        assert_eq!(n, 0);

        client_handle.await.unwrap();
    }

    // ── QuicConnector / connect_quic tests ──────────────────────

    #[tokio::test]
    async fn given_invalid_address_when_connect_quic_then_returns_error() {
        let result = connect_quic("999.999.999.999:9999").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_unparseable_address_when_connect_quic_then_returns_error() {
        let result = connect_quic("not-an-address").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_connector_when_connect_to_invalid_addr_then_returns_error() {
        let connector = QuicConnector;
        let result = Connector::connect(&connector, "999.999.999.999:9999").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_connector_when_connected_then_peer_address_matches() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector;

        let server_handle = tokio::spawn(async move { listener.accept().await.unwrap() });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        let client_peer = client_conn.peer();
        assert!(client_peer.starts_with("127.0.0.1:"));

        // Drive the control channel so the server side can complete.
        client_conn.write_all(b"ping").await.unwrap();

        let mut server_conn = server_handle.await.unwrap();
        let server_peer = server_conn.peer();
        assert!(server_peer.starts_with("127.0.0.1:"));

        let mut buf = [0u8; 64];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");
    }

    #[tokio::test]
    async fn given_connector_when_client_writes_then_server_reads() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector;

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 128];
            let n = conn.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        client_conn
            .write_all(b"hello from quic client")
            .await
            .unwrap();
        client_conn.shutdown().await.unwrap();

        let received = server_handle.await.unwrap();
        assert_eq!(received, "hello from quic client");
    }

    #[tokio::test]
    async fn given_connector_when_server_writes_then_client_reads() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector;

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            // Server must read from client first (client opens the
            // control stream, server accepts it).
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
            conn.write_all(b"hello from quic server").await.unwrap();
            // Keep connection alive until the client has finished reading.
            let _ = done_rx.await;
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        // Client writes first to establish the control stream.
        client_conn.write_all(b"hi").await.unwrap();

        let mut buf = [0u8; 128];
        let n = client_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from quic server");

        let _ = done_tx.send(());
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_connector_when_bidirectional_exchange_then_both_sides_receive() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector;

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            let received = String::from_utf8_lossy(&buf[..n]).to_string();
            conn.write_all(b"pong").await.unwrap();
            // Keep connection alive until the client has finished reading.
            let _ = done_rx.await;
            received
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        client_conn.write_all(b"ping").await.unwrap();

        let mut buf = [0u8; 64];
        let n = client_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"pong");

        let _ = done_tx.send(());
        let server_received = server_handle.await.unwrap();
        assert_eq!(server_received, "ping");
    }

    #[tokio::test]
    async fn given_connector_when_client_opens_data_stream_then_server_accepts_it() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector;

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            // Read control channel.
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"ctrl");
            // Accept data stream.
            let mut data_stream = conn.accept_stream().await.unwrap();
            let mut data_buf = [0u8; 128];
            let n = data_stream.read(&mut data_buf).await.unwrap();
            String::from_utf8_lossy(&data_buf[..n]).to_string()
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        // Write on control channel.
        client_conn.write_all(b"ctrl").await.unwrap();
        // Open a data stream and write.
        let mut data_stream = client_conn.open_stream().await.unwrap();
        data_stream.write_all(b"file-payload").await.unwrap();
        data_stream.shutdown().await.unwrap();

        let received = server_handle.await.unwrap();
        assert_eq!(received, "file-payload");
    }

    #[tokio::test]
    async fn given_connector_when_server_opens_data_stream_then_client_accepts_it() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector;

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            // Read control channel.
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
            // Open a data stream from server to client.
            let mut data_stream = conn.open_stream().await.unwrap();
            data_stream.write_all(b"server-file-data").await.unwrap();
            data_stream.shutdown().await.unwrap();
            // Keep connection alive until the client has finished reading.
            let _ = done_rx.await;
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        // Write on control channel to establish it.
        client_conn.write_all(b"go").await.unwrap();
        // Accept data stream from server.
        let mut data_stream = client_conn.accept_stream().await.unwrap();
        let mut buf = [0u8; 128];
        let n = data_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"server-file-data");

        let _ = done_tx.send(());
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_connector_when_large_payload_on_data_stream_then_data_intact() {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector;

        let payload: Vec<u8> = (0u32..131_072).map(|i| (i % 251) as u8).collect();
        let payload_clone = payload.clone();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();

            let mut data_stream = conn.accept_stream().await.unwrap();
            let mut received = Vec::new();
            let mut data_buf = [0u8; 4096];
            loop {
                let n = data_stream.read(&mut data_buf).await.unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&data_buf[..n]);
            }
            received
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        client_conn.write_all(b"go").await.unwrap();

        let mut data_stream = client_conn.open_stream().await.unwrap();
        data_stream.write_all(&payload_clone).await.unwrap();
        data_stream.shutdown().await.unwrap();

        let received = server_handle.await.unwrap();
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
    }

    // ---------------------------------------------------------------
    //  QUIC + Session functional integration tests
    //
    //  These exercise real QUIC transport (not mocks) through the full
    //  Session state machine to catch transport-level issues such as
    //  the CONNECTION_CLOSE race that occurs when a session drops its
    //  QuicConnection before the peer has read outstanding data.
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

    /// Helper: create a QUIC listener + client connection pair for session tests.
    async fn quic_session_pair(
        send_dir: &std::path::Path,
        recv_dir: &std::path::Path,
    ) -> (SessionHandle, SessionHandle) {
        let factory = QuicListenerFactory::new().unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        // Spawn accept in background so connect doesn't deadlock.
        let recv_path = recv_dir.to_path_buf();
        let server_task = tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            Session::spawn(conn, Role::Server, "QuicServer".into(), recv_path)
        });

        let client_conn = connect_quic(&addr).await.unwrap();
        let handle_c = Session::spawn(
            client_conn,
            Role::Client,
            "QuicClient".into(),
            send_dir.to_path_buf(),
        );

        let handle_s = server_task.await.unwrap();
        (handle_c, handle_s)
    }

    /// Given a real QUIC connection, when a file is transferred and
    /// accepted, then both sides emit TransferComplete and Finished
    /// without any errors (no CONNECTION_CLOSE race).
    #[tokio::test]
    async fn given_quic_transfer_when_accepted_then_both_sides_complete_without_error() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        let content = b"QUIC integration test payload - no race!";
        tokio::fs::write(send_dir.path().join("send_file.txt"), content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) =
            quic_session_pair(send_dir.path(), recv_dir.path()).await;

        // Wait for handshake on both sides.
        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        // Client: send file.
        let req = SendRequest {
            transfer_id: "quic-xfer-1".into(),
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

        // Server: accept offer.
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::TransferOffered { .. })
        })
        .await;
        handle_s
            .cmd_tx
            .send(SessionCmd::RespondToOffer {
                transfer_id: "quic-xfer-1".into(),
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

    /// Given a real QUIC connection, when a large file (256 KB) is
    /// transferred, then progress events are emitted and data arrives
    /// intact.
    #[tokio::test]
    async fn given_quic_large_file_when_transferred_then_progress_emitted_and_data_intact() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        #[allow(clippy::cast_possible_truncation)]
        let content: Vec<u8> = (0..262_144u32).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(send_dir.path().join("send_file.txt"), &content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) =
            quic_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "quic-big".into(),
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
                transfer_id: "quic-big".into(),
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

    /// Given a real QUIC connection, when the server denies the
    /// transfer, then the client sees TransferDenied and both sides
    /// remain operational (no crash, no error).
    #[tokio::test]
    async fn given_quic_transfer_when_denied_then_client_sees_denied_without_error() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        let content = b"deny-me over QUIC";
        tokio::fs::write(send_dir.path().join("send_file.txt"), content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) =
            quic_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "quic-deny".into(),
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
                transfer_id: "quic-deny".into(),
                accept: false,
            })
            .await
            .unwrap();

        let ev = wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::TransferDenied { .. })
        })
        .await;
        assert!(
            matches!(ev, SessionEvent::TransferDenied { transfer_id } if transfer_id == "quic-deny")
        );

        // Clean shutdown.
        let _ = handle_c.cmd_tx.send(SessionCmd::Cancel).await;
        let _ = handle_s.cmd_tx.send(SessionCmd::Cancel).await;
    }

    /// Given a real QUIC connection, when the handshake completes, then
    /// both sides report the correct peer device name.
    #[tokio::test]
    async fn given_quic_connection_when_handshake_completes_then_peer_names_are_correct() {
        let dir = tempfile::tempdir().unwrap();

        let (mut handle_c, mut handle_s) = quic_session_pair(dir.path(), dir.path()).await;

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
        assert_eq!(client_sees, "QuicServer");

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
        assert_eq!(server_sees, "QuicClient");

        let _ = handle_c.cmd_tx.send(SessionCmd::Cancel).await;
        let _ = handle_s.cmd_tx.send(SessionCmd::Cancel).await;
    }

    /// Given a real QUIC connection, when the client cancels mid-session,
    /// then both sides terminate (Finished or Error) without panicking.
    #[tokio::test]
    async fn given_quic_session_when_client_cancels_then_both_sides_terminate() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        let content = b"cancel me over QUIC";
        tokio::fs::write(send_dir.path().join("send_file.txt"), content)
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) =
            quic_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        // Send an offer, then cancel before the server responds.
        let req = SendRequest {
            transfer_id: "quic-cancel".into(),
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

        // Cancel client.
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

    /// Given a real QUIC connection with an empty file, when transferred,
    /// then both sides complete and the empty file is created.
    #[tokio::test]
    async fn given_quic_empty_file_when_transferred_then_both_sides_complete() {
        let send_dir = tempfile::tempdir().unwrap();
        let recv_dir = tempfile::tempdir().unwrap();

        tokio::fs::write(send_dir.path().join("send_file.txt"), b"")
            .await
            .unwrap();

        let (mut handle_c, mut handle_s) =
            quic_session_pair(send_dir.path(), recv_dir.path()).await;

        wait_for_session_event(&mut handle_c.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;
        wait_for_session_event(&mut handle_s.event_rx, |e| {
            matches!(e, SessionEvent::Connected { .. })
        })
        .await;

        let req = SendRequest {
            transfer_id: "quic-empty".into(),
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
                transfer_id: "quic-empty".into(),
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
