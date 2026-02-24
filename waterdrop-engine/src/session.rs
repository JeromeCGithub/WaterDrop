use tracing::info;

use waterdrop_core::listener::Connection;

/// Trait for handling accepted connections.
///
/// The engine calls [`handle`](SessionHandler::handle) for every inbound
/// connection, each in its own spawned task. Implementations carry the
/// actual protocol logic (authentication, transfer, …).
///
/// Wrap shared state in the implementor itself — the engine clones an
/// `Arc<H>` for every spawned task.
pub trait SessionHandler<C: Connection>: Send + Sync + 'static {
    fn handle(&self, conn: C) -> impl Future<Output = ()> + Send;
}

/// A minimal session handler that logs the peer address and drops the
/// connection. Useful as a starting point or for smoke-testing the engine.
pub struct ConcreteSessionHandler;

impl<C: Connection> SessionHandler<C> for ConcreteSessionHandler {
    async fn handle(&self, conn: C) {
        let peer = conn.peer();
        info!(peer = %peer, "Session started (no protocol implemented yet)");
    }
}
