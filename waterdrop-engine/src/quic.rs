use std::sync::Arc;

use anyhow::Context;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use tracing::{debug, info};

use waterdrop_core::transport::{Connection, DataStream, Listener, ListenerFactory};
use waterdrop_core::tls;

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

/// A QUIC connection backed by a single bi-directional stream.
///
/// Stream establishment is lazy: the underlying bi-directional stream is
/// accepted on the first call to [`read`](Connection::read) or
/// [`write_all`](Connection::write_all), not during
/// [`accept`](QuicListener::accept).  This mirrors TCP where `accept()`
/// returns as soon as the handshake completes.
///
/// Additional data streams can be opened via [`open_stream`](Connection::open_stream)
/// or accepted via [`accept_stream`](Connection::accept_stream).  These are
/// independent of the control channel and can be shut down or dropped
/// without affecting control traffic.
pub struct QuicConnection {
    connection: quinn::Connection,

    send: Option<quinn::SendStream>,
    recv: Option<quinn::RecvStream>,
    peer_addr: String,
}

impl QuicConnection {
    async fn ensure_streams(&mut self) -> anyhow::Result<()> {
        if self.send.is_none() {
            let (send, recv) = self
                .connection
                .accept_bi()
                .await
                .context("failed to accept bi-directional QUIC stream")?;
            self.send = Some(send);
            self.recv = Some(recv);
        }
        Ok(())
    }
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
            self.ensure_streams().await?;
            self.recv
                .as_mut()
                .expect("streams initialized")
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
            self.ensure_streams().await?;
            self.send
                .as_mut()
                .expect("streams initialized")
                .write_all(buf)
                .await
                .context("failed to write to QUIC stream")
        }
    }

    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            if let Some(ref mut send) = self.send {
                send.finish().context("failed to finish QUIC send stream")
            } else {
                Ok(())
            }
        }
    }

    fn open_stream(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<Self::DataStream>> + Send + '_ {
        async move {
            self.ensure_streams().await?;
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
            self.ensure_streams().await?;
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

            Ok(QuicConnection {
                connection,
                send: None,
                recv: None,
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
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint().unwrap();

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let remote = connection.remote_address();
            let _ = done_rx.await;
            remote
        });

        let server_conn = listener.accept().await.unwrap();
        let server_peer: std::net::SocketAddr = server_conn.peer().parse().unwrap();

        let _ = done_tx.send(());
        let client_remote = client_handle.await.unwrap();

        assert_eq!(server_peer.ip(), client_remote.ip());
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
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client1 = make_client_endpoint().unwrap();
        let client2 = make_client_endpoint().unwrap();

        let (done_tx1, done_rx1) = tokio::sync::oneshot::channel::<()>();
        let (done_tx2, done_rx2) = tokio::sync::oneshot::channel::<()>();

        let h1 = tokio::spawn(async move {
            let _conn = client1.connect(addr, "localhost").unwrap().await.unwrap();
            let _ = done_rx1.await;
        });

        let h2 = tokio::spawn(async move {
            let _conn = client2.connect(addr, "localhost").unwrap().await.unwrap();
            let _ = done_rx2.await;
        });

        let conn1 = listener.accept().await.unwrap();
        let conn2 = listener.accept().await.unwrap();

        assert_ne!(conn1.peer(), conn2.peer());

        let _ = done_tx1.send(());
        let _ = done_tx2.send(());
        let _ = h1.await;
        let _ = h2.await;
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
}
