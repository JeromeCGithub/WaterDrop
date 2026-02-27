mod command;
mod event;
mod ui;

use std::path::PathBuf;

use clap::Parser;
use tokio::io::BufReader;
use tokio::sync::mpsc;
use tracing_subscriber::{EnvFilter, fmt};

use waterdrop_engine::engine::{Engine, EngineCmd, EngineConfig};
use waterdrop_engine::tcp::{TcpConnector, TcpListenerFactory};

use crate::command::{handle_connect_cmd, handle_pending_offer};
use crate::event::{PendingOffer, spawn_event_printer};
use crate::ui::{print_banner, print_help, print_prompt, read_line};

/// WaterDrop — peer-to-peer file transfer.
///
/// Starts a dual-mode engine that can accept incoming connections
/// (server) and initiate outgoing connections (client) at the same
/// time.  An interactive prompt lets you connect to peers, send files,
/// and accept or deny incoming transfers.
#[derive(Parser, Debug)]
#[command(name = "waterdrop", version, about)]
struct Args {
    /// Address to listen on for incoming connections.
    #[arg(short, long, default_value = "0.0.0.0:4242")]
    listen: String,

    /// Human-readable name for this device.
    #[arg(short, long, default_value = "WaterDrop-CLI")]
    name: String,

    /// Directory where received files are stored.
    #[arg(short, long, default_value = "/tmp/waterdrop")]
    receive_dir: PathBuf,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Tracing goes to stderr so it doesn't mix with the interactive
    // prompt on stdout.  Default to "warn" for library crates so
    // only the CLI's own output is visible.
    fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("waterdrop_cli=info,warn")),
        )
        .init();

    if let Err(e) = std::fs::create_dir_all(&args.receive_dir) {
        eprintln!(
            "Failed to create receive directory {}: {e}",
            args.receive_dir.display()
        );
        std::process::exit(1);
    }

    let config = EngineConfig {
        device_name: args.name.clone(),
        receive_dir: args.receive_dir.clone(),
    };

    let engine = Engine;
    let handle = engine.start(TcpListenerFactory, TcpConnector, config);

    // Subscribe to engine events.
    let events_rx = handle.events_tx.subscribe();

    // Channel for pending offers that need user input.
    let (pending_tx, mut pending_rx) = mpsc::unbounded_channel::<PendingOffer>();

    // Spawn the event printer.
    spawn_event_printer(events_rx, pending_tx);

    let cmd_tx = handle.cmd_tx.clone();

    if let Err(e) = cmd_tx
        .send(EngineCmd::StartAccepting {
            addr: args.listen.clone(),
        })
        .await
    {
        eprintln!("Failed to start listener: {e}");
        std::process::exit(1);
    }

    // Small delay so the "Listening on ..." event prints before
    // the banner.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // ── Banner ──────────────────────────────────────────────────
    print_banner(&args.listen, &args.name, &args.receive_dir);
    print_help();
    print_prompt();

    // ── Interactive prompt loop ─────────────────────────────────
    let mut stdin = BufReader::new(tokio::io::stdin());

    loop {
        tokio::select! {
            biased;

            // Check for pending offers that need a response.
            Some(offer) = pending_rx.recv() => {
                handle_pending_offer(&offer, &cmd_tx, &mut stdin).await;
                print_prompt();
            }

            // Read user input.
            line = read_line(&mut stdin) => {
                let Some(line) = line else {
                    // EOF — shut down.
                    break;
                };

                if line.is_empty() {
                    print_prompt();
                    continue;
                }

                let parts: Vec<&str> = line.splitn(3, ' ').collect();

                match parts[0] {
                    "connect" => {
                        handle_connect_cmd(&parts, &cmd_tx, &args.receive_dir).await;
                    }
                    "accept" | "deny" => {
                        println!("  ℹ No pending offer right now.  Wait for an incoming transfer.");
                    }
                    "help" | "?" => {
                        print_help();
                    }
                    "quit" | "exit" | "q" => {
                        break;
                    }
                    other => {
                        println!("  ❓ Unknown command: \"{other}\".  Type 'help' for usage.");
                    }
                }

                print_prompt();
            }
        }
    }

    println!("\n  Shutting down...");
    let _ = cmd_tx.send(EngineCmd::ShutDown).await;
    // Give sessions a moment to clean up.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    println!("  Bye! 👋");
}
