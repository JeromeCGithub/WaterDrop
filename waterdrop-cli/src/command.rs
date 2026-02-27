use std::path::{Path, PathBuf};

use tokio::io::BufReader;
use tokio::sync::mpsc;

use waterdrop_engine::engine::EngineCmd;
use waterdrop_engine::session::{SendRequest, SessionCmd};

use crate::event::PendingOffer;
use crate::ui::{format_size, read_line};

/// Generates a unique transfer ID based on the current timestamp.
pub fn generate_transfer_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("xfer-{ts}")
}

/// Handles the `connect <addr> <file>` command.
pub async fn handle_connect_cmd(
    parts: &[&str],
    cmd_tx: &mpsc::Sender<EngineCmd>,
    receive_dir: &Path,
) {
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

/// Handles a pending incoming transfer offer by prompting the user to accept or deny.
pub async fn handle_pending_offer(
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
