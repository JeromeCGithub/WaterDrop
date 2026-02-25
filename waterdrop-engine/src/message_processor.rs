#![allow(clippy::unnecessary_wraps)] // handlers are stubs — they will return Err once implemented

use tracing::{debug, warn};

use waterdrop_core::protocol::{Frame, MessageType};

/// Dispatches a fully decoded frame to the appropriate handler.
///
/// # Errors
///
/// Returns an error if the handler signals an unrecoverable protocol
/// violation that should tear down the session.
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

fn handle_hello(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received HELLO");
    // TODO: deserialize payload, verify signature, send HELLO_ACK
    let _ = payload;
    Ok(())
}

fn handle_hello_ack(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received HELLO_ACK");
    // TODO: deserialize payload, update session state
    let _ = payload;
    Ok(())
}

fn handle_pair_with_code(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received PAIR_WITH_CODE");
    // TODO: validate code, persist device, send PAIR_RESULT
    let _ = payload;
    Ok(())
}

fn handle_pair_with_password(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received PAIR_WITH_PASSWORD");
    // TODO: validate password, persist device, send PAIR_RESULT
    let _ = payload;
    Ok(())
}

fn handle_pair_result(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received PAIR_RESULT");
    // TODO: deserialize payload, update session state
    let _ = payload;
    Ok(())
}

fn handle_transfer_offer(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received TRANSFER_OFFER");
    // TODO: deserialize offer, validate, emit event or auto-accept, send TRANSFER_DECISION
    let _ = payload;
    Ok(())
}

fn handle_transfer_decision(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received TRANSFER_DECISION");
    // TODO: deserialize decision, start or abort file streaming
    let _ = payload;
    Ok(())
}

fn handle_transfer_done(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    debug!(peer = %peer, "Received TRANSFER_DONE");
    // TODO: deserialize result, emit completion event
    let _ = payload;
    Ok(())
}

fn handle_error(peer: &str, payload: &[u8]) -> anyhow::Result<()> {
    warn!(peer = %peer, "Received ERROR frame");
    // TODO: deserialize error, log details, emit event
    let _ = payload;
    Ok(())
}
