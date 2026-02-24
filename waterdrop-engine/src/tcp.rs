use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net;
use tracing::{debug, info};

use waterdrop_core::listener::{Connection, Listener, ListenerFactory};

/// A TCP connection wrapping a [`tokio::net::TcpStream`].
pub struct TcpConnection {
    stream: net::TcpStream,
    peer_addr: String,
}

impl TcpConnection {
    /// Returns a shared reference to the underlying stream.
    pub fn stream(&self) -> &net::TcpStream {
        &self.stream
    }

    /// Returns a mutable reference to the underlying stream.
    pub fn stream_mut(&mut self) -> &mut net::TcpStream {
        &mut self.stream
    }
}

impl Connection for TcpConnection {
    fn peer(&self) -> String {
        self.peer_addr.clone()
    }

    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = anyhow::Result<usize>> + Send + 'a {
        async move {
            self.stream
                .read(buf)
                .await
                .context("failed to read from TCP connection")
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
                .context("failed to write to TCP connection")
        }
    }

    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            self.stream
                .shutdown()
                .await
                .context("failed to shut down TCP connection")
        }
    }
}

/// A TCP listener wrapping a [`tokio::net::TcpListener`].
pub struct TcpListener {
    inner: net::TcpListener,
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
            Ok(TcpConnection { stream, peer_addr })
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
            let inner = net::TcpListener::bind(addr)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn when_binding_invalid_address_expect_error() {
        let factory = TcpListenerFactory;
        let result = factory.bind("999.999.999.999:0").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn when_client_connects_expect_peer_matches_client_address() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let client = net::TcpStream::connect(&addr).await.unwrap();
        let client_local = client.local_addr().unwrap().to_string();

        let conn = listener.accept().await.unwrap();
        assert_eq!(conn.peer(), client_local);
    }

    #[tokio::test]
    async fn when_two_clients_connect_expect_distinct_peers() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let _c1 = net::TcpStream::connect(&addr).await.unwrap();
        let conn1 = listener.accept().await.unwrap();

        let _c2 = net::TcpStream::connect(&addr).await.unwrap();
        let conn2 = listener.accept().await.unwrap();

        assert_ne!(
            conn1.peer(),
            conn2.peer(),
            "each connection should have a distinct peer address"
        );
    }

    #[tokio::test]
    async fn when_accessing_stream_expect_both_accessors_return_same_peer() {
        let factory = TcpListenerFactory;
        let mut listener = factory.bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr();

        let _client = net::TcpStream::connect(&addr).await.unwrap();
        let mut conn = listener.accept().await.unwrap();

        let peer_shared = conn.stream().peer_addr().unwrap().to_string();
        let peer_mut = conn.stream_mut().peer_addr().unwrap().to_string();
        assert_eq!(peer_shared, peer_mut);
        assert_eq!(conn.peer(), peer_shared);
    }
}
