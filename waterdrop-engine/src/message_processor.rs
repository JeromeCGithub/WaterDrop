use tracing::{debug, warn};

use waterdrop_core::protocol::{Frame, MessageType};

/// Processes a fully decoded frame by dispatching to the appropriate handler
/// based on the message type.
///
/// Returns `Ok(())` when the message was handled successfully, or `Err(..)` if
/// the session should be torn down (e.g. unrecoverable protocol error).
pub fn process_frame(peer: &str, frame: &Frame) -> anyhow::Result<()> {
    debug!(
        peer = %peer,
        msg_type = ?frame.header.msg_type,
        payload_len = frame.header.payload_length,
        "Processing frame"
    );

    match frame.header.msg_type {
        MessageType::Hello => handle_hello(peer, &frame.payload),
        MessageType::HelloAck => handle_hello_ack(peer, &frame.payload),
        MessageType::PairWithCode => handle_pair_with_code(peer, &frame.payload),
        MessageType::PairWithPassword => handle_pair_with_password(peer, &frame.payload),
        MessageType::PairResult => handle_pair_result(peer, &frame.payload),
        MessageType::TransferOffer => handle_transfer_offer(peer, &frame.payload),
        MessageType::TransferDecision => handle_transfer_decision(peer, &frame.payload),
        MessageType::TransferDone => handle_transfer_done(peer, &frame.payload),
        MessageType::Error => handle_error(peer, &frame.payload),
    }
}

/// Handles an incoming HELLO message from a sender.
///
/// Responsibilities (to be implemented):
/// - Deserialize the JSON payload (device_id, device_name, timestamp, nonce, signature).
/// - Look up the device in the paired-devices store.
/// - Verify the Ed25519 signature.
/// - Respond with HELLO_ACK.
fn handle_hello(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received HELLO");
    // TODO: deserialize payload, verify signature, send HELLO_ACK
    let _ = payload;
    Ok(())
}

/// Handles an incoming HELLO_ACK message from a receiver.
///
/// Responsibilities (to be implemented):
/// - Deserialize the JSON payload (ok, paired).
/// - Transition the session state depending on whether pairing is required.
fn handle_hello_ack(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received HELLO_ACK");
    // TODO: deserialize payload, update session state
    let _ = payload;
    Ok(())
}

/// Handles an incoming PAIR_WITH_CODE message from a sender.
///
/// Responsibilities (to be implemented):
/// - Verify that pairing mode is active and not expired.
/// - Validate the pairing code against the displayed code.
/// - If valid, persist the new device in the paired-devices store.
/// - Respond with PAIR_RESULT.
fn handle_pair_with_code(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received PAIR_WITH_CODE");
    // TODO: validate code, persist device, send PAIR_RESULT
    let _ = payload;
    Ok(())
}

/// Handles an incoming PAIR_WITH_PASSWORD message from a sender (NAS/headless).
///
/// Responsibilities (to be implemented):
/// - Verify that pairing mode is active and not expired.
/// - Compare the password hash (with rate limiting).
/// - If valid, persist the new device in the paired-devices store.
/// - Respond with PAIR_RESULT.
fn handle_pair_with_password(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received PAIR_WITH_PASSWORD");
    // TODO: validate password, persist device, send PAIR_RESULT
    let _ = payload;
    Ok(())
}

/// Handles an incoming PAIR_RESULT message from a receiver.
///
/// Responsibilities (to be implemented):
/// - Deserialize the JSON payload (ok).
/// - If ok, transition the session to allow transfers.
fn handle_pair_result(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received PAIR_RESULT");
    // TODO: deserialize payload, update session state
    let _ = payload;
    Ok(())
}

/// Handles an incoming TRANSFER_OFFER message from a sender.
///
/// Responsibilities (to be implemented):
/// - Verify the sender is paired/authenticated.
/// - Deserialize the JSON payload (transfer_id, filename, size_bytes, sha256_hex).
/// - Sanitize the filename.
/// - If auto_accept is enabled, accept immediately; otherwise emit an event
///   for the UI/CLI to prompt the user.
/// - Respond with TRANSFER_DECISION.
fn handle_transfer_offer(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received TRANSFER_OFFER");
    // TODO: deserialize offer, validate, emit event or auto-accept, send TRANSFER_DECISION
    let _ = payload;
    Ok(())
}

/// Handles an incoming TRANSFER_DECISION message from a receiver.
///
/// Responsibilities (to be implemented):
/// - Deserialize the JSON payload (transfer_id, accept).
/// - If accepted, begin streaming the file bytes.
/// - If denied, clean up and optionally close the connection.
fn handle_transfer_decision(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received TRANSFER_DECISION");
    // TODO: deserialize decision, start or abort file streaming
    let _ = payload;
    Ok(())
}

/// Handles an incoming TRANSFER_DONE message from a receiver.
///
/// Responsibilities (to be implemented):
/// - Deserialize the JSON payload (transfer_id, ok, stored_filename, reason).
/// - Emit a completion event to the UI/CLI.
/// - Clean up transfer state.
fn handle_transfer_done(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received TRANSFER_DONE");
    // TODO: deserialize result, emit completion event
    let _ = payload;
    Ok(())
}

/// Handles an incoming ERROR message from either side.
///
/// Responsibilities (to be implemented):
/// - Deserialize the JSON payload (code, message).
/// - Log the error and emit an event to the UI/CLI.
/// - Decide whether the session should continue or be torn down.
fn handle_error(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    warn!(peer = %peer, "Received ERROR frame");
    // TODO: deserialize error, log details, emit event
    let _ = payload;
    Ok(())
}
