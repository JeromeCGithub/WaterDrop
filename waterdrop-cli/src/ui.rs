use std::io::Write;
use std::path::Path;

/// Formats a byte count into a human-readable string (B, KiB, MiB, GiB).
#[allow(clippy::cast_precision_loss)]
pub fn format_size(bytes: u64) -> String {
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

/// Prints the interactive prompt marker (`> `) and flushes stdout.
pub fn print_prompt() {
    print!("\n> ");
    let _ = std::io::stdout().flush();
}

/// Prints the startup banner with device info.
pub fn print_banner(listen_addr: &str, device_name: &str, receive_dir: &Path) {
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

/// Prints available commands.
pub fn print_help() {
    println!();
    println!("  Commands:");
    println!("    connect <addr> <file>   Connect to a peer and send a file");
    println!("    help                    Show this help");
    println!("    quit                    Shut down and exit");
    println!();
    println!("  When an incoming transfer offer arrives you will be");
    println!("  prompted to accept or deny it.");
}

/// Reads one trimmed line from the given buffered stdin reader.
/// Returns `None` on EOF or read error.
pub async fn read_line(reader: &mut tokio::io::BufReader<tokio::io::Stdin>) -> Option<String> {
    use tokio::io::AsyncBufReadExt;

    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}
