use tokio::sync::{broadcast, mpsc};

use waterdrop_engine::engine::EngineEvent;
use waterdrop_engine::session::SessionEvent;

use crate::ui::{format_size, print_prompt};

/// Pending offer waiting for user input.
pub struct PendingOffer {
    pub session_id: u64,
    pub transfer_id: String,
    pub filename: String,
    pub size_bytes: u64,
}

/// Spawns a task that listens for engine events and prints them.
///
/// When a `TransferOffered` event arrives it is pushed into
/// `pending_tx` so the main prompt loop can ask the user.
pub fn spawn_event_printer(
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
