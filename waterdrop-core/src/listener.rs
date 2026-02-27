use std::future::Future;

/// A transport-agnostic byte stream.
///
/// Both the control channel of a [`Connection`] and the sub-streams
/// returned by [`Connection::open_stream`] / [`Connection::accept_stream`]
/// implement this trait, so higher-level code can read and write without
/// knowing whether it is talking over QUIC, yamux-over-TCP, or an
/// in-memory pipe.
pub trait DataStream: Send + 'static {
    /// Reads bytes into `buf`, returning how many bytes were read.
    ///
    /// Returns `Ok(0)` when the remote end has closed the stream.
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = anyhow::Result<usize>> + Send + 'a;

    /// Writes the entirety of `buf` to the stream.
    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = anyhow::Result<()>> + Send + 'a;

    /// Shuts down the write half of the stream, signalling to the
    /// remote end that no more data will be sent.
    fn shutdown(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send + '_; // TODO: remote reference as this will not live after shutdown.
}

/// A transport-agnostic multiplexed connection.
///
/// Provides identity (`peer`), byte-level I/O on the **control channel**
/// (`read`, `write_all`, `shutdown`), and the ability to open or accept
/// additional **data streams** so the engine can work with any transport
/// without knowing whether it is TCP, QUIC, an in-memory pipe, etc.
///
/// The control channel is the first logical stream established after
/// the connection is accepted — it carries protocol frames (HELLO,
/// TRANSFER_OFFER, …).  Data streams are opened on demand for bulk
/// file transfer and can be cancelled independently of the control
/// channel.
pub trait Connection: Send + 'static {
    /// The concrete sub-stream type produced by [`open_stream`](Connection::open_stream)
    /// and [`accept_stream`](Connection::accept_stream).
    type DataStream: DataStream;

    /// Returns a human-readable identifier for the remote end
    /// (e.g. `"127.0.0.1:54321"`).
    fn peer(&self) -> String;

    // ── Control channel ─────────────────────────────────────────

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

    // ── Data streams ────────────────────────────────────────────

    /// Opens a new bidirectional data stream (sender side).
    ///
    /// The returned stream is independent of the control channel and
    /// can be shut down or dropped without affecting control traffic.
    fn open_stream(&mut self)
    -> impl Future<Output = anyhow::Result<Self::DataStream>> + Send + '_;

    /// Accepts the next inbound data stream (receiver side).
    ///
    /// The returned stream is independent of the control channel and
    /// can be shut down or dropped without affecting control traffic.
    fn accept_stream(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<Self::DataStream>> + Send + '_;
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

/// Factory for creating outbound [`Connection`]s (client side).
///
/// This is the counterpart to [`ListenerFactory`]: where the listener
/// accepts inbound connections (server), the connector initiates outbound
/// connections (client).  The engine uses this trait so that client-mode
/// sessions work with any transport (TCP, QUIC, in-memory, …).
pub trait Connector: Send + Sync + 'static {
    /// The concrete connection type produced by [`connect`](Connector::connect).
    type Conn: Connection;

    /// Opens a new outbound connection to the given address.
    fn connect<'a>(
        &'a self,
        addr: &'a str,
    ) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send + 'a;
}
