use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use tracing::{debug, info, warn};

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
/// The server certificate is persisted to `cert_dir` so that clients can
/// recognise the server across restarts (TOFU — Trust On First Use).
pub struct QuicListenerFactory {
    server_config: quinn::ServerConfig,
}

impl QuicListenerFactory {
    /// Creates a new factory that loads (or generates and saves) a server
    /// certificate in `cert_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if certificate I/O or TLS configuration fails.
    pub fn new(cert_dir: &Path) -> anyhow::Result<Self> {
        let server_config = build_server_config(cert_dir)?;
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
/// `trust_store` is the directory where TOFU fingerprints are stored.
/// On first contact with a server, its certificate fingerprint is saved.
/// On subsequent connections, the server's certificate must match.
///
/// # Errors
///
/// Returns an error if the address cannot be parsed, the client endpoint
/// cannot be created, or the QUIC handshake fails.
pub async fn connect_quic(addr: &str, trust_store: &Path) -> anyhow::Result<QuicConnection> {
    let socket_addr: std::net::SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid connect address: {addr}"))?;

    let bind_addr: std::net::SocketAddr = "0.0.0.0:0"
        .parse()
        .context("failed to parse ephemeral bind address")?;
    let mut endpoint =
        quinn::Endpoint::client(bind_addr).context("failed to create QUIC client endpoint")?;
    endpoint.set_default_client_config(tofu_client_config(trust_store)?);

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
/// Certificate fingerprints are persisted in `trust_store` using TOFU
/// (Trust On First Use).
pub struct QuicConnector {
    trust_store: PathBuf,
}

impl QuicConnector {
    #[must_use]
    pub fn new(trust_store: &Path) -> Self {
        Self {
            trust_store: trust_store.to_path_buf(),
        }
    }
}

impl Connector for QuicConnector {
    type Conn = QuicConnection;

    fn connect<'a>(
        &'a self,
        addr: &'a str,
    ) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send + 'a {
        connect_quic(addr, &self.trust_store)
    }
}

fn build_server_config(certs_dir: &Path) -> anyhow::Result<quinn::ServerConfig> {
    let pair = tls::load_or_generate_cert(certs_dir, &["localhost"])?;

    let fp = tls::fingerprint_sha256(&pair.cert_der);
    info!(fingerprint = %fp, "server certificate fingerprint");

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

/// Builds a QUIC client config with TOFU certificate verification.
///
/// TLS handshake signatures are fully verified. On first connection to a
/// server, its certificate fingerprint is stored in `trust_store`. On
/// subsequent connections, the fingerprint must match or the handshake is
/// rejected.
///
/// # Errors
///
/// Returns an error if the trust store directory cannot be created or
/// the TLS client configuration fails to build.
pub fn tofu_client_config(trust_store: &Path) -> anyhow::Result<quinn::ClientConfig> {
    let verifier = TofuVerifier::new(trust_store)?;

    let mut tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();

    tls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_config: QuicClientConfig = tls_config
        .try_into()
        .context("failed to build QUIC client config")?;

    Ok(quinn::ClientConfig::new(Arc::new(quic_config)))
}

/// Trust-On-First-Use certificate verifier.
///
/// - TLS handshake signatures are always verified cryptographically.
/// - On first connection, the server certificate fingerprint is persisted
///   to a file named `<fingerprint>.fp` inside the trust store directory.
/// - On subsequent connections, the presented certificate must have a
///   fingerprint that exists in the store.
#[derive(Debug)]
struct TofuVerifier {
    store_dir: PathBuf,
}

impl TofuVerifier {
    fn new(store_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(store_dir)
            .with_context(|| format!("failed to create trust store {}", store_dir.display()))?;
        Ok(Self {
            store_dir: store_dir.to_path_buf(),
        })
    }

    fn fingerprint_path(&self, fingerprint: &str) -> PathBuf {
        self.store_dir.join(format!("{fingerprint}.fp"))
    }

    fn is_trusted(&self, fingerprint: &str) -> bool {
        self.fingerprint_path(fingerprint).exists()
    }

    fn has_any_fingerprint(&self) -> bool {
        std::fs::read_dir(&self.store_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "fp"))
            })
            .unwrap_or(false)
    }

    fn trust(&self, fingerprint: &str) -> Result<(), rustls::Error> {
        std::fs::write(self.fingerprint_path(fingerprint), fingerprint.as_bytes())
            .map_err(|e| rustls::Error::General(format!("failed to persist TOFU fingerprint: {e}")))
    }
}

impl rustls::client::danger::ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fingerprint = tls::fingerprint_sha256(end_entity.as_ref());

        if self.is_trusted(&fingerprint) {
            debug!(fingerprint = %fingerprint, "server certificate matches trusted fingerprint");
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }

        if self.has_any_fingerprint() {
            warn!(
                fingerprint = %fingerprint,
                "server certificate fingerprint not in trust store — possible impersonation"
            );
            return Err(rustls::Error::General(format!(
                "TOFU: server certificate fingerprint {fingerprint} is not trusted"
            )));
        }

        info!(fingerprint = %fingerprint, "first contact — trusting server certificate (TOFU)");
        self.trust(&fingerprint)?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
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

    fn make_server(cert_dir: &Path) -> anyhow::Result<QuicListenerFactory> {
        QuicListenerFactory::new(cert_dir)
    }

    fn make_client_endpoint(trust_store: &Path) -> anyhow::Result<quinn::Endpoint> {
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
            .context("failed to create client endpoint")?;
        endpoint.set_default_client_config(tofu_client_config(trust_store)?);
        Ok(endpoint)
    }

    #[tokio::test]
    async fn given_invalid_address_when_binding_then_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let factory = make_server(dir.path()).unwrap();
        let result = factory.bind("999.999.999.999:0").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_valid_address_when_binding_then_returns_listener_with_local_addr() {
        let dir = tempfile::tempdir().unwrap();
        let factory = make_server(dir.path()).unwrap();
        let listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();
        assert!(addr.starts_with("127.0.0.1:"));
        let port: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();
        assert_ne!(port, 0);
    }

    #[tokio::test]
    async fn given_client_connects_when_accepted_then_peer_address_matches() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let server_handle = tokio::spawn(async move { listener.accept().await.unwrap() });

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        let client_peer: std::net::SocketAddr = client_conn.peer().parse().unwrap();

        client_conn.write_all(b"ping").await.unwrap();

        let mut server_conn = server_handle.await.unwrap();
        let server_peer: std::net::SocketAddr = server_conn.peer().parse().unwrap();

        let mut buf = [0u8; 16];
        let n = server_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");

        assert_eq!(server_peer.ip(), client_peer.ip());
    }

    #[tokio::test]
    async fn given_client_sends_data_when_server_reads_then_data_matches() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint(trust_dir.path()).unwrap();

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
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr: std::net::SocketAddr = listener.local_addr().parse().unwrap();

        let client_endpoint = make_client_endpoint(trust_dir.path()).unwrap();

        let client_handle = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();

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
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"bye");
            let n = conn.read(&mut buf).await.unwrap();
            assert_eq!(n, 0);
        });

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        client_conn.write_all(b"bye").await.unwrap();
        client_conn.shutdown().await.unwrap();

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_two_clients_when_accepted_then_peers_are_distinct() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let td = trust_dir.path().to_path_buf();
        let addr_clone = addr.clone();
        let c1 = tokio::spawn(async move {
            let mut conn = connect_quic(&addr_clone, &td).await.unwrap();
            conn.write_all(b"c1").await.unwrap();
            conn
        });

        let td = trust_dir.path().to_path_buf();
        let addr_clone = addr.clone();
        let c2 = tokio::spawn(async move {
            let mut conn = connect_quic(&addr_clone, &td).await.unwrap();
            conn.write_all(b"c2").await.unwrap();
            conn
        });

        let conn1 = listener.accept().await.unwrap();
        let conn2 = listener.accept().await.unwrap();

        assert_ne!(conn1.peer(), conn2.peer());

        let _ = c1.await.unwrap();
        let _ = c2.await.unwrap();
    }

    #[tokio::test]
    async fn given_large_payload_when_sent_and_received_then_data_is_intact() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        #[allow(clippy::cast_possible_truncation)]
        let payload: Vec<u8> = (0u32..65_536).map(|i| (i % 251) as u8).collect();
        let payload_clone = payload.clone();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
            }
            received
        });

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        client_conn.write_all(&payload_clone).await.unwrap();
        client_conn.shutdown().await.unwrap();

        let received = server_handle.await.unwrap();
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn given_bidirectional_exchange_when_both_sides_send_then_both_receive() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            let received = String::from_utf8_lossy(&buf[..n]).to_string();
            conn.write_all(b"pong").await.unwrap();
            let _ = done_rx.await;
            received
        });

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        client_conn.write_all(b"ping").await.unwrap();

        let mut buf = [0u8; 64];
        let n = client_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"pong");

        let _ = done_tx.send(());
        let server_received = server_handle.await.unwrap();
        assert_eq!(server_received, "ping");
    }

    #[tokio::test]
    async fn given_client_opens_data_stream_when_server_accepts_then_data_flows() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"ctrl");

            let mut data_stream = conn.accept_stream().await.unwrap();
            let mut data_buf = [0u8; 128];
            let n = data_stream.read(&mut data_buf).await.unwrap();
            String::from_utf8_lossy(&data_buf[..n]).to_string()
        });

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        client_conn.write_all(b"ctrl").await.unwrap();

        let mut data_stream = client_conn.open_stream().await.unwrap();
        data_stream.write_all(b"stream-payload").await.unwrap();
        data_stream.shutdown().await.unwrap();

        let received = server_handle.await.unwrap();
        assert_eq!(received, "stream-payload");
    }

    #[tokio::test]
    async fn given_server_opens_data_stream_when_client_accepts_then_data_flows() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
            let mut data_stream = conn.open_stream().await.unwrap();
            data_stream.write_all(b"server-data").await.unwrap();
            data_stream.shutdown().await.unwrap();
            let _ = done_rx.await;
        });

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        client_conn.write_all(b"go").await.unwrap();

        let mut data_stream = client_conn.accept_stream().await.unwrap();
        let mut buf = [0u8; 128];
        let n = data_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"server-data");

        let _ = done_tx.send(());
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_large_payload_on_data_stream_when_received_then_data_is_intact() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        #[allow(clippy::cast_possible_truncation)]
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

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        client_conn.write_all(b"go").await.unwrap();

        let mut data_stream = client_conn.open_stream().await.unwrap();
        data_stream.write_all(&payload_clone).await.unwrap();
        data_stream.shutdown().await.unwrap();

        let received = server_handle.await.unwrap();
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn given_data_stream_closed_when_read_then_returns_zero() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();

            let mut data_stream = conn.accept_stream().await.unwrap();
            let mut data_buf = [0u8; 64];
            let n = data_stream.read(&mut data_buf).await.unwrap();
            assert_eq!(&data_buf[..n], b"data");
            let n = data_stream.read(&mut data_buf).await.unwrap();
            assert_eq!(n, 0);
        });

        let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        client_conn.write_all(b"ctrl").await.unwrap();

        let mut data_stream = client_conn.open_stream().await.unwrap();
        data_stream.write_all(b"data").await.unwrap();
        data_stream.shutdown().await.unwrap();

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_invalid_address_when_connect_quic_then_returns_error() {
        let trust_dir = tempfile::tempdir().unwrap();
        let result = connect_quic("999.999.999.999:9999", trust_dir.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_unparseable_address_when_connect_quic_then_returns_error() {
        let trust_dir = tempfile::tempdir().unwrap();
        let result = connect_quic("not-an-address", trust_dir.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_connector_when_connect_to_invalid_addr_then_returns_error() {
        let trust_dir = tempfile::tempdir().unwrap();
        let connector = QuicConnector::new(trust_dir.path());
        let result = Connector::connect(&connector, "999.999.999.999:9999").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn given_connector_when_connected_then_peer_address_matches() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector::new(trust_dir.path());

        let server_handle = tokio::spawn(async move { listener.accept().await.unwrap() });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        let client_peer = client_conn.peer();
        assert!(client_peer.starts_with("127.0.0.1:"));

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
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector::new(trust_dir.path());

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
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector::new(trust_dir.path());

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
            conn.write_all(b"hello from quic server").await.unwrap();
            let _ = done_rx.await;
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        client_conn.write_all(b"hi").await.unwrap();

        let mut buf = [0u8; 128];
        let n = client_conn.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from quic server");

        let _ = done_tx.send(());
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_connector_when_bidirectional_exchange_then_both_sides_receive() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector::new(trust_dir.path());

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            let received = String::from_utf8_lossy(&buf[..n]).to_string();
            conn.write_all(b"pong").await.unwrap();
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
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector::new(trust_dir.path());

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"ctrl");
            let mut data_stream = conn.accept_stream().await.unwrap();
            let mut data_buf = [0u8; 128];
            let n = data_stream.read(&mut data_buf).await.unwrap();
            String::from_utf8_lossy(&data_buf[..n]).to_string()
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        client_conn.write_all(b"ctrl").await.unwrap();
        let mut data_stream = client_conn.open_stream().await.unwrap();
        data_stream.write_all(b"file-payload").await.unwrap();
        data_stream.shutdown().await.unwrap();

        let received = server_handle.await.unwrap();
        assert_eq!(received, "file-payload");
    }

    #[tokio::test]
    async fn given_connector_when_server_opens_data_stream_then_client_accepts_it() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector::new(trust_dir.path());

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
            let mut data_stream = conn.open_stream().await.unwrap();
            data_stream.write_all(b"server-file-data").await.unwrap();
            data_stream.shutdown().await.unwrap();
            let _ = done_rx.await;
        });

        let mut client_conn = Connector::connect(&connector, &addr).await.unwrap();
        client_conn.write_all(b"go").await.unwrap();
        let mut data_stream = client_conn.accept_stream().await.unwrap();
        let mut buf = [0u8; 128];
        let n = data_stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"server-file-data");

        let _ = done_tx.send(());
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_connector_when_large_payload_on_data_stream_then_data_intact() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();
        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let connector = QuicConnector::new(trust_dir.path());

        #[allow(clippy::cast_possible_truncation)]
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

    #[tokio::test]
    async fn given_same_server_cert_when_client_reconnects_then_tofu_succeeds() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();

        let factory = make_server(cert_dir.path()).unwrap();

        for _ in 0..3 {
            let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr();

            let server_handle = tokio::spawn(async move {
                let mut conn = listener.accept().await.unwrap();
                let mut buf = [0u8; 64];
                let _n = conn.read(&mut buf).await.unwrap();
            });

            let mut client_conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
            client_conn.write_all(b"hi").await.unwrap();
            client_conn.shutdown().await.unwrap();

            server_handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn given_trusted_server_when_different_server_connects_then_tofu_rejects() {
        let cert_dir_a = tempfile::tempdir().unwrap();
        let cert_dir_b = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();

        let factory_a = make_server(cert_dir_a.path()).unwrap();
        let mut listener_a = factory_a.bind("127.0.0.1:0").await.unwrap();
        let addr_a = listener_a.local_addr();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener_a.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
        });

        let mut conn_a = connect_quic(&addr_a, trust_dir.path()).await.unwrap();
        conn_a.write_all(b"hi").await.unwrap();
        conn_a.shutdown().await.unwrap();
        server_handle.await.unwrap();

        let factory_b = make_server(cert_dir_b.path()).unwrap();
        let mut listener_b = factory_b.bind("127.0.0.1:0").await.unwrap();
        let addr_b = listener_b.local_addr();

        let server_handle = tokio::spawn(async move {
            let _ = listener_b.accept().await;
        });

        let result = connect_quic(&addr_b, trust_dir.path()).await;
        assert!(
            result.is_err(),
            "TOFU should reject a different server cert"
        );

        server_handle.abort();
    }

    #[tokio::test]
    async fn given_persisted_cert_when_factory_recreated_then_same_cert_used() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();

        let factory1 = make_server(cert_dir.path()).unwrap();
        let mut listener1 = factory1.bind("127.0.0.1:0").await.unwrap();
        let addr1 = listener1.local_addr();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener1.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
        });

        let mut conn = connect_quic(&addr1, trust_dir.path()).await.unwrap();
        conn.write_all(b"first").await.unwrap();
        conn.shutdown().await.unwrap();
        server_handle.await.unwrap();

        let factory2 = make_server(cert_dir.path()).unwrap();
        let mut listener2 = factory2.bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr();

        let server_handle = tokio::spawn(async move {
            let mut conn = listener2.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
        });

        let mut conn = connect_quic(&addr2, trust_dir.path()).await.unwrap();
        conn.write_all(b"second").await.unwrap();
        conn.shutdown().await.unwrap();
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn given_no_prior_trust_when_first_connect_then_fingerprint_file_created() {
        let cert_dir = tempfile::tempdir().unwrap();
        let trust_dir = tempfile::tempdir().unwrap();

        let factory = make_server(cert_dir.path()).unwrap();
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let fp_files_before: Vec<_> = std::fs::read_dir(trust_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "fp"))
            .collect();
        assert!(fp_files_before.is_empty());

        let server_handle = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _n = conn.read(&mut buf).await.unwrap();
        });

        let mut conn = connect_quic(&addr, trust_dir.path()).await.unwrap();
        conn.write_all(b"hi").await.unwrap();
        conn.shutdown().await.unwrap();
        server_handle.await.unwrap();

        let fp_files_after: Vec<_> = std::fs::read_dir(trust_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "fp"))
            .collect();
        assert_eq!(fp_files_after.len(), 1);
    }
}
