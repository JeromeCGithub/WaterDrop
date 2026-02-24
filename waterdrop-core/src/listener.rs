use std::future::Future;

/// A transport-agnostic connection.
///
/// Provides identity (`peer`) and byte-level I/O (`read`, `write_all`,
/// `shutdown`) so the engine can work with any transport without knowing
/// whether it is TCP, QUIC, an in-memory pipe, etc.
pub trait Connection: Send + 'static {
    /// Returns a human-readable identifier for the remote end
    /// (e.g. `"127.0.0.1:54321"`).
    fn peer(&self) -> String;

    /// Reads bytes into `buf`, returning how many bytes were read.
    ///
    /// Returns `Ok(0)` when the remote end has closed the connection.
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = anyhow::Result<usize>> + Send + 'a;

    /// Writes the entirety of `buf` to the connection.
    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = anyhow::Result<()>> + Send + 'a;

    /// Shuts down the write half of the connection, signalling to the
    /// remote end that no more data will be sent.
    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_;
}

/// An async listener that accepts incoming [`Connection`]s.
pub trait Listener: Send + 'static {
    /// The concrete connection type produced by [`accept`](Listener::accept).
    type Conn: Connection;

    /// Returns the local address the listener is bound to
    /// (e.g. `"0.0.0.0:9000"`).
    fn local_addr(&self) -> String;

    /// Waits for and accepts the next inbound connection.
    fn accept(&mut self) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send + '_;
}

/// Factory for creating [`Listener`] instances.
///
/// Separating creation from usage lets the engine remain generic: pass a
/// [`TcpListenerFactory`](crate) in production and a fake/in-memory factory
/// in tests.
pub trait ListenerFactory: Send + Sync + 'static {
    /// The concrete listener type produced by [`bind`](ListenerFactory::bind).
    type L: Listener;

    /// Binds a new listener to the given address.
    ///
    /// Use `"<ip>:0"` to let the OS assign an available port.
    fn bind<'a>(
        &'a self,
        addr: &'a str,
    ) -> impl Future<Output = anyhow::Result<Self::L>> + Send + 'a;
}
