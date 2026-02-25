use bytes::BytesMut;
use tokio::select;
use tracing::{info, warn};

use waterdrop_core::{listener::Connection, protocol::try_decode_frame};

use crate::message_processor::process_frame;

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
    async fn handle(&self, mut conn: C) {
        let peer = conn.peer();
        info!(peer = %peer, "Session started");

        let mut buf = [0u8; 2048];
        let mut accum = BytesMut::with_capacity(4096);

        loop {
            select! {
                res = conn.read(&mut buf) => match res {
                    Ok(0) => {
                        info!(peer = %peer, "Connection closed by peer");
                        break;
                    }
                    Ok(n) => {
                        accum.extend_from_slice(&buf[..n]);

                        // Drain all complete frames from the accumulation buffer.
                        loop {
                            match try_decode_frame(&mut accum) {
                                Ok(Some(frame)) => {
                                    if let Err(e) = process_frame(&peer, &frame) {
                                        warn!(peer = %peer, error = %e, "Error processing frame");
                                        return;
                                    }
                                }
                                Ok(None) => {
                                    // Incomplete frame — not enough data yet.
                                    // Leave the bytes in accum and wait for the
                                    // next read to bring more.
                                    break;
                                }
                                Err(e) => {
                                    // Protocol violation (bad magic, unsupported
                                    // version, unknown type, oversized payload).
                                    // The connection is in an unrecoverable state.
                                    warn!(peer = %peer, error = %e, "Protocol error, closing connection");
                                    // TODO: send ERROR frame back before closing
                                    return;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        warn!(peer = %peer, error = %err, "Read error, closing connection");
                        return;
                    }
                }
            }
        }
    }
}
