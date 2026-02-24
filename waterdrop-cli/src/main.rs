use std::sync::Arc;

use tokio::signal;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use waterdrop_engine::engine::{Engine, EngineCmd, EngineEvent};
use waterdrop_engine::session::ConcreteSessionHandler;
use waterdrop_engine::tcp::TcpListenerFactory;

#[tokio::main]
async fn main() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("WaterDrop CLI starting up");

    let engine = Engine;
    let handle = engine.start(TcpListenerFactory, Arc::new(ConcreteSessionHandler));

    let mut events_rx = handle.events_tx.subscribe();

    // Tell the engine to start accepting connections.
    if let Err(e) = handle
        .cmd_tx
        .send(EngineCmd::StartAccepting {
            addr: "127.0.0.1:9000".to_string(),
        })
        .await
    {
        warn!("Failed to send StartAccepting command: {e}");
        return;
    }

    // Listen for events until Ctrl+C.
    tokio::spawn(async move {
        loop {
            match events_rx.recv().await {
                Ok(EngineEvent::Accepting { addr }) => {
                    info!("Engine is now accepting connections on {addr}");
                }
                Ok(EngineEvent::AcceptingStopped) => {
                    info!("Engine stopped accepting connections");
                }
                Ok(EngineEvent::ConnectionAccepted { peer }) => {
                    info!("New connection from {peer}");
                }
                Ok(EngineEvent::Error { message }) => {
                    warn!("Engine error: {message}");
                }
                Err(e) => {
                    warn!("Event channel error: {e}");
                    break;
                }
            }
        }
    });

    // Wait for Ctrl+C, then shut down gracefully.
    match signal::ctrl_c().await {
        Ok(()) => info!("Ctrl+C received, shutting down"),
        Err(e) => warn!("Failed to listen for Ctrl+C: {e}"),
    }

    if let Err(e) = handle.cmd_tx.send(EngineCmd::ShutDown).await {
        warn!("Failed to send ShutDown command: {e}");
    }

    info!("WaterDrop CLI shut down");
}
