use std::path::{Path, PathBuf};

use clap::Parser;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, mpsc};
use tracing_subscriber::{EnvFilter, fmt};

use waterdrop_engine::engine::{Engine, EngineCmd, EngineConfig, EngineEvent};
use waterdrop_engine::session::{SendRequest, SessionCmd, SessionEvent};
use waterdrop_engine::tcp::{TcpConnector, TcpListenerFactory};

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

#[allow(clippy::cast_precision_loss)]
fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn generate_transfer_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("xfer-{ts}")
}

/// Reads one trimmed line from stdin.  Returns `None` on EOF.
async fn read_line(reader: &mut BufReader<tokio::io::Stdin>) -> Option<String> {
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

fn print_prompt() {
    use std::io::Write;
    print!("\n> ");
    let _ = std::io::stdout().flush();
}

fn print_banner(listen_addr: &str, device_name: &str, receive_dir: &Path) {
    println!();
    println!("╔══════════════════════════════════════════════════════╗");
    println!("║              🌊  WaterDrop  CLI  🌊                 ║");
    println!("╠══════════════════════════════════════════════════════╣");
    println!("║  Device  : {device_name:<41} ║");
    println!("║  Listen  : {listen_addr:<41} ║");
    println!("║  Save to : {:<41} ║", receive_dir.display().to_string());
    println!("╚══════════════════════════════════════════════════════╝");
    println!();
}

fn print_help() {
    println!();
    println!("  Commands:");
    println!("    connect <addr> <file>   Connect to a peer and send a file");
    println!("    help                    Show this help");
    println!("    quit                    Shut down and exit");
    println!();
    println!("  When an incoming transfer offer arrives you will be");
    println!("  prompted to accept or deny it.");
}

/// Pending offer waiting for user input.
struct PendingOffer {
    session_id: u64,
    transfer_id: String,
    filename: String,
    size_bytes: u64,
}

/// Spawns a task that listens for engine events and prints them.
///
/// When a `TransferOffered` event arrives it is pushed into
/// `pending_tx` so the main prompt loop can ask the user.
fn spawn_event_printer(
    mut events_rx: broadcast::Receiver<EngineEvent>,
    pending_tx: mpsc::UnboundedSender<PendingOffer>,
) {
    tokio::spawn(async move {
        loop {
            match events_rx.recv().await {
                Ok(EngineEvent::Accepting { addr }) => {
                    println!("\n  ✔ Listening on {addr}");
                    print_prompt();
                }
                Ok(EngineEvent::AcceptingStopped) => {
                    println!("\n  ⏹ Stopped accepting connections");
                    print_prompt();
                }
                Ok(EngineEvent::SessionCreated { session_id, peer }) => {
                    println!("\n  📡 Session #{session_id} — connected to {peer}");
                    print_prompt();
                }
                Ok(EngineEvent::SessionEvent { session_id, event }) => {
                    handle_session_event(session_id, event, &pending_tx);
                }
                Ok(EngineEvent::Error { message }) => {
                    println!("\n  ❌ Engine error: {message}");
                    print_prompt();
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    println!("\n  ⚠ Missed {n} events");
                    print_prompt();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn handle_session_event(
    session_id: u64,
    event: SessionEvent,
    pending_tx: &mpsc::UnboundedSender<PendingOffer>,
) {
    match event {
        SessionEvent::Connected { peer_device_name } => {
            println!(
                "\n  🤝 Session #{session_id}: handshake complete with \"{peer_device_name}\""
            );
            print_prompt();
        }
        SessionEvent::TransferOffered {
            transfer_id,
            filename,
            size_bytes,
        } => {
            println!();
            println!("  📥 Session #{session_id}: incoming transfer offer!");
            println!(
                "     File: {filename}  ({size})",
                size = format_size(size_bytes)
            );
            println!("     Type 'accept' or 'deny' to respond.");
            let _ = pending_tx.send(PendingOffer {
                session_id,
                transfer_id,
                filename,
                size_bytes,
            });
            print_prompt();
        }
        SessionEvent::TransferAccepted { transfer_id } => {
            println!(
                "\n  ✅ Session #{session_id}: transfer {transfer_id} accepted by peer — sending file..."
            );
            print_prompt();
        }
        SessionEvent::TransferDenied { transfer_id } => {
            println!("\n  🚫 Session #{session_id}: transfer {transfer_id} denied by peer");
            print_prompt();
        }
        SessionEvent::TransferProgress {
            bytes_transferred,
            total_bytes,
            ..
        } => {
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            if total_bytes > 0 {
                let pct = (bytes_transferred as f64 / total_bytes as f64) * 100.0;
                // Overwrite the same line with \r for a compact progress bar.
                let bar_width: usize = 30;
                let filled = ((pct / 100.0) * bar_width as f64) as usize;
                let empty = bar_width - filled;
                print!(
                    "\r  📊 #{session_id} [{}{}] {pct:>5.1}%  {sent} / {total}",
                    "█".repeat(filled),
                    "░".repeat(empty),
                    sent = format_size(bytes_transferred),
                    total = format_size(total_bytes),
                );
                let _ = std::io::Write::flush(&mut std::io::stdout());
                if bytes_transferred >= total_bytes {
                    println!();
                }
            }
        }
        SessionEvent::TransferComplete { transfer_id } => {
            println!("\n  🎉 Session #{session_id}: transfer {transfer_id} complete!");
            print_prompt();
        }
        SessionEvent::Error { message } => {
            println!("\n  ❌ Session #{session_id}: error — {message}");
            print_prompt();
        }
        SessionEvent::Finished => {
            println!("  👋 Session #{session_id}: session finished");
            print_prompt();
        }
    }
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

async fn handle_connect_cmd(parts: &[&str], cmd_tx: &mpsc::Sender<EngineCmd>, receive_dir: &Path) {
    if parts.len() < 3 {
        println!("  Usage: connect <addr> <file_path>");
        println!("  Example: connect 192.168.1.42:4242 /home/user/photo.jpg");
        return;
    }

    let addr = parts[1];
    let file_path = PathBuf::from(parts[2]);

    // Validate the file exists and is readable.
    let metadata = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(e) => {
            println!("  ❌ Cannot read file {}: {e}", file_path.display());
            return;
        }
    };

    if !metadata.is_file() {
        println!("  ❌ {} is not a regular file", file_path.display());
        return;
    }

    let size_bytes = metadata.len();
    let filename = file_path
        .file_name()
        .map_or_else(|| "unnamed".into(), |n| n.to_string_lossy().to_string());

    let transfer_id = generate_transfer_id();

    println!(
        "  📤 Connecting to {addr} to send \"{filename}\" ({size})...",
        size = format_size(size_bytes)
    );

    // We need to copy the file into the engine's receive_dir/send_file.txt
    // because the session currently reads from that hardcoded path.
    let send_staging = receive_dir.join("send_file.txt");
    if let Err(e) = tokio::fs::copy(&file_path, &send_staging).await {
        println!("  ❌ Failed to stage file for sending: {e}");
        return;
    }

    let send_request = SendRequest {
        transfer_id: transfer_id.clone(),
        file_path: send_staging,
        filename,
        size_bytes,
        sha256_hex: "not-computed".into(),
    };

    if let Err(e) = cmd_tx
        .send(EngineCmd::Connect {
            addr: addr.to_string(),
            send_request: Some(send_request),
        })
        .await
    {
        println!("  ❌ Failed to send connect command: {e}");
    }
}

async fn handle_pending_offer(
    offer: &PendingOffer,
    cmd_tx: &mpsc::Sender<EngineCmd>,
    stdin: &mut BufReader<tokio::io::Stdin>,
) {
    println!();
    println!(
        "  📥 Accept \"{filename}\" ({size}) from session #{sid}?",
        filename = offer.filename,
        size = format_size(offer.size_bytes),
        sid = offer.session_id,
    );
    println!("     Type 'accept' or 'deny':");

    loop {
        print!("  [accept/deny] > ");
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let Some(answer) = read_line(stdin).await else {
            return;
        };

        match answer.to_lowercase().as_str() {
            "accept" | "a" | "yes" | "y" => {
                println!("  ✅ Accepting transfer...");
                let _ = cmd_tx
                    .send(EngineCmd::SessionCmd {
                        session_id: offer.session_id,
                        cmd: SessionCmd::RespondToOffer {
                            transfer_id: offer.transfer_id.clone(),
                            accept: true,
                        },
                    })
                    .await;
                return;
            }
            "deny" | "d" | "no" | "n" => {
                println!("  🚫 Denying transfer.");
                let _ = cmd_tx
                    .send(EngineCmd::SessionCmd {
                        session_id: offer.session_id,
                        cmd: SessionCmd::RespondToOffer {
                            transfer_id: offer.transfer_id.clone(),
                            accept: false,
                        },
                    })
                    .await;
                return;
            }
            _ => {
                println!("  Please type 'accept' or 'deny'.");
            }
        }
    }
}
