//! Session functional tests over the in-memory mock transport.
//!
//! These tests exercise the full [`Session`] state machine (handshake,
//! offer, accept/deny, transfer, progress, cancellation) without
//! touching the network.  They are fast, deterministic, and verify the
//! protocol-level behaviour independently of the transport layer.

mod common;

use std::time::Duration;

use common::{
    MockConnection, collect_events_until, make_send_request, mock_session_pair, wait_for_event,
};
use waterdrop_core::protocol::{
    HelloAckPayload, MessageType, TransferOfferPayload, encode_payload_frame, try_decode_frame,
};
use waterdrop_core::transport::Connection;
use waterdrop_engine::session::{Role, SendRequest, Session, SessionCmd, SessionEvent};

/// Given a client and server, when they complete the handshake, then
/// both emit Connected with each other's device name.
#[tokio::test]
async fn given_client_and_server_when_handshake_then_both_emit_connected() {
    let dir = tempfile::tempdir().unwrap();
    let (mut hc, mut hs) = mock_session_pair(dir.path(), dir.path());

    let ev_c = wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    assert!(
        matches!(ev_c, SessionEvent::Connected { peer_device_name } if peer_device_name == "MockServer")
    );

    let ev_s = wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    assert!(
        matches!(ev_s, SessionEvent::Connected { peer_device_name } if peer_device_name == "MockClient")
    );

    let _ = hc.cmd_tx.send(SessionCmd::Cancel).await;
    let _ = hs.cmd_tx.send(SessionCmd::Cancel).await;
}

/// Given a handshake, when completed, then both sides report the
/// correct peer device name.
#[tokio::test]
async fn given_handshake_when_completed_then_peer_names_are_correct() {
    let (conn_c, conn_s) = MockConnection::pair("c", "s");
    let dir = tempfile::tempdir().unwrap();

    let mut hs = Session::spawn(
        conn_s,
        Role::Server,
        "MyServer".into(),
        dir.path().to_path_buf(),
    );
    let mut hc = Session::spawn(
        conn_c,
        Role::Client,
        "MyClient".into(),
        dir.path().to_path_buf(),
    );

    let ev_c = wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    let SessionEvent::Connected {
        peer_device_name: client_sees,
    } = ev_c
    else {
        unreachable!()
    };
    assert_eq!(client_sees, "MyServer");

    let ev_s = wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    let SessionEvent::Connected {
        peer_device_name: server_sees,
    } = ev_s
    else {
        unreachable!()
    };
    assert_eq!(server_sees, "MyClient");

    let _ = hc.cmd_tx.send(SessionCmd::Cancel).await;
    let _ = hs.cmd_tx.send(SessionCmd::Cancel).await;
}

/// Given a server session, when it receives a wrong first frame (not
/// HELLO), then the session errors.
#[tokio::test]
async fn given_server_when_receives_wrong_first_frame_then_errors() {
    let (mut conn_c, conn_s) = MockConnection::pair("c", "s");
    let dir = tempfile::tempdir().unwrap();

    let mut hs = Session::spawn(conn_s, Role::Server, "S".into(), dir.path().to_path_buf());

    let offer = TransferOfferPayload {
        transfer_id: "bad".into(),
        filename: "x".into(),
        size_bytes: 0,
        sha256_hex: String::new(),
    };
    let frame = encode_payload_frame(MessageType::TransferOffer, &offer).unwrap();
    Connection::write_all(&mut conn_c, &frame).await.unwrap();

    let ev = wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Error { .. } | SessionEvent::Finished)
    })
    .await;
    assert!(matches!(
        ev,
        SessionEvent::Error { .. } | SessionEvent::Finished
    ));
}

/// Given a client session, when the server rejects the HELLO, then
/// the client session errors.
#[tokio::test]
async fn given_client_when_hello_rejected_then_errors() {
    let (conn_c, mut conn_s) = MockConnection::pair("c", "s");
    let dir = tempfile::tempdir().unwrap();

    let mut hc = Session::spawn(conn_c, Role::Client, "C".into(), dir.path().to_path_buf());

    // Manually act as server: read HELLO, send rejecting HELLO_ACK.
    let mut buf = [0u8; 1024];
    let mut accum = bytes::BytesMut::new();
    loop {
        let n = Connection::read(&mut conn_s, &mut buf).await.unwrap();
        accum.extend_from_slice(&buf[..n]);
        if try_decode_frame(&mut accum).unwrap().is_some() {
            break;
        }
    }

    let ack = HelloAckPayload {
        ok: false,
        device_name: "S".into(),
    };
    let frame = encode_payload_frame(MessageType::HelloAck, &ack).unwrap();
    Connection::write_all(&mut conn_s, &frame).await.unwrap();

    let ev = wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Error { .. } | SessionEvent::Finished)
    })
    .await;
    assert!(matches!(
        ev,
        SessionEvent::Error { .. } | SessionEvent::Finished
    ));
}

// ---------------------------------------------------------------------------
//  Transfer — accept
// ---------------------------------------------------------------------------

/// Given a file transfer, when the server accepts, then the file is
/// received intact and both sides emit TransferComplete.
#[tokio::test]
async fn given_file_transfer_when_accepted_then_file_received() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"Hello, WaterDrop! This is a test file.";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request(
        "xfer-1",
        send_dir.path(),
        "received.txt",
        content.len() as u64,
    );
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    let ev = wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    let SessionEvent::TransferOffered {
        transfer_id: tid, ..
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(tid, "xfer-1");

    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: tid,
            accept: true,
        })
        .await
        .unwrap();

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;

    let received = tokio::fs::read(recv_dir.path().join("received.txt"))
        .await
        .unwrap();
    assert_eq!(received, content);
}

/// Given a completed transfer, when done, then both sides emit
/// TransferComplete and Finished without any Error events.
#[tokio::test]
async fn given_completed_transfer_when_done_then_both_sides_finish_without_error() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"clean-finish test payload";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request("finish-1", send_dir.path(), "out.txt", content.len() as u64);
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "finish-1".into(),
            accept: true,
        })
        .await
        .unwrap();

    // Client events.
    let client_events = collect_events_until(
        &mut hc.event_rx,
        |e| matches!(e, SessionEvent::Finished),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        client_events
            .iter()
            .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
        "client must see TransferComplete"
    );
    assert!(
        client_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Finished)),
        "client must see Finished"
    );
    assert!(
        !client_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Error { .. })),
        "client must NOT see Error, got: {client_events:?}"
    );

    // Server events.
    let server_events = collect_events_until(
        &mut hs.event_rx,
        |e| matches!(e, SessionEvent::Finished),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        server_events
            .iter()
            .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
        "server must see TransferComplete"
    );
    assert!(
        server_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Finished)),
        "server must see Finished"
    );
    assert!(
        !server_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Error { .. })),
        "server must NOT see Error, got: {server_events:?}"
    );

    let received = tokio::fs::read(recv_dir.path().join("out.txt"))
        .await
        .unwrap();
    assert_eq!(received, content);
}

/// Given a transfer offer, when accepted, then the client receives
/// TransferAccepted before any TransferProgress events.
#[tokio::test]
async fn given_accepted_offer_when_transfer_starts_then_accepted_before_progress() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"order-test payload with enough bytes to generate progress";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request(
        "order-1",
        send_dir.path(),
        "order.txt",
        content.len() as u64,
    );
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "order-1".into(),
            accept: true,
        })
        .await
        .unwrap();

    let events = collect_events_until(
        &mut hc.event_rx,
        |e| matches!(e, SessionEvent::TransferComplete { .. }),
        Duration::from_secs(5),
    )
    .await;

    let accepted_pos = events
        .iter()
        .position(|e| matches!(e, SessionEvent::TransferAccepted { .. }));
    let first_progress_pos = events
        .iter()
        .position(|e| matches!(e, SessionEvent::TransferProgress { .. }));

    assert!(accepted_pos.is_some(), "must see TransferAccepted");
    if let Some(progress_pos) = first_progress_pos {
        assert!(
            accepted_pos.unwrap() < progress_pos,
            "TransferAccepted (pos {accepted_pos:?}) must come before first TransferProgress (pos {progress_pos})",
        );
    }
}

/// Given a transfer offer, when the server sees TransferOffered, the
/// event carries the correct filename and size.
#[tokio::test]
async fn given_transfer_offer_when_server_receives_event_then_metadata_matches() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"metadata check content";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = SendRequest {
        transfer_id: "meta-1".into(),
        file_path: send_dir.path().join("send_file.txt"),
        filename: "my_doc.pdf".into(),
        size_bytes: content.len() as u64,
        sha256_hex: "n/a".into(),
    };
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    let ev = wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;

    let SessionEvent::TransferOffered {
        transfer_id,
        filename,
        size_bytes,
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(transfer_id, "meta-1");
    assert_eq!(filename, "my_doc.pdf");
    assert_eq!(size_bytes, content.len() as u64);

    let _ = hc.cmd_tx.send(SessionCmd::Cancel).await;
    let _ = hs.cmd_tx.send(SessionCmd::Cancel).await;
}

// ---------------------------------------------------------------------------
//  Transfer — deny
// ---------------------------------------------------------------------------

/// Given a transfer offer, when the server denies it, then the client
/// sees TransferDenied.
#[tokio::test]
async fn given_transfer_offer_when_denied_then_client_sees_denied() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"data";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request(
        "xfer-deny",
        send_dir.path(),
        "nope.txt",
        content.len() as u64,
    );
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;

    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "xfer-deny".into(),
            accept: false,
        })
        .await
        .unwrap();

    let ev = wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferDenied { .. })
    })
    .await;
    assert!(
        matches!(ev, SessionEvent::TransferDenied { transfer_id } if transfer_id == "xfer-deny")
    );

    let _ = hc.cmd_tx.send(SessionCmd::Cancel).await;
    let _ = hs.cmd_tx.send(SessionCmd::Cancel).await;
}

/// Given a denied transfer, when the client offers again, then the
/// second transfer succeeds.
#[tokio::test]
async fn given_denied_transfer_when_second_offer_sent_then_transfer_succeeds() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"second-attempt data";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    // Wait for handshake.
    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    // --- First offer (denied) ---
    let req1 = make_send_request(
        "xfer-retry-1",
        send_dir.path(),
        "out.txt",
        content.len() as u64,
    );
    hc.cmd_tx
        .send(SessionCmd::Transfer { req: req1 })
        .await
        .unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "xfer-retry-1".into(),
            accept: false,
        })
        .await
        .unwrap();

    let denied = wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferDenied { .. })
    })
    .await;
    assert!(
        matches!(denied, SessionEvent::TransferDenied { transfer_id } if transfer_id == "xfer-retry-1")
    );

    // --- Second offer (accepted) ---
    let req2 = make_send_request(
        "xfer-retry-2",
        send_dir.path(),
        "out.txt",
        content.len() as u64,
    );
    hc.cmd_tx
        .send(SessionCmd::Transfer { req: req2 })
        .await
        .unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "xfer-retry-2".into(),
            accept: true,
        })
        .await
        .unwrap();

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;

    let received = tokio::fs::read(recv_dir.path().join("out.txt"))
        .await
        .unwrap();
    assert_eq!(received, content);
}

// ---------------------------------------------------------------------------
//  Transfer — large file + progress
// ---------------------------------------------------------------------------

/// Given a 64 KB file, when transferred, then progress events are
/// emitted and the file arrives intact.
#[tokio::test]
async fn given_large_file_when_transferred_then_progress_emitted_and_intact() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    #[allow(clippy::cast_possible_truncation)]
    let content: Vec<u8> = (0..65_536u32).map(|i| (i % 256) as u8).collect();
    tokio::fs::write(send_dir.path().join("send_file.txt"), &content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request(
        "xfer-large",
        send_dir.path(),
        "big.bin",
        content.len() as u64,
    );
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "xfer-large".into(),
            accept: true,
        })
        .await
        .unwrap();

    let client_events = collect_events_until(
        &mut hc.event_rx,
        |e| matches!(e, SessionEvent::TransferComplete { .. }),
        Duration::from_secs(10),
    )
    .await;
    let progress_count = client_events
        .iter()
        .filter(|e| matches!(e, SessionEvent::TransferProgress { .. }))
        .count();
    assert!(progress_count > 0, "expected at least one progress event");

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;

    let received = tokio::fs::read(recv_dir.path().join("big.bin"))
        .await
        .unwrap();
    assert_eq!(received, content);
}

/// Given a 256 KB file, when transferred, then multiple progress events
/// are emitted on both sides and data arrives intact.
#[tokio::test]
async fn given_256kb_file_when_transferred_then_progress_on_both_sides_and_data_intact() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    #[allow(clippy::cast_possible_truncation)]
    let content: Vec<u8> = (0..262_144u32).map(|i| (i % 251) as u8).collect();
    tokio::fs::write(send_dir.path().join("send_file.txt"), &content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request(
        "big-256k",
        send_dir.path(),
        "big256k.bin",
        content.len() as u64,
    );
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "big-256k".into(),
            accept: true,
        })
        .await
        .unwrap();

    let c_events = collect_events_until(
        &mut hc.event_rx,
        |e| matches!(e, SessionEvent::TransferComplete { .. }),
        Duration::from_secs(15),
    )
    .await;
    let c_progress = c_events
        .iter()
        .filter(|e| matches!(e, SessionEvent::TransferProgress { .. }))
        .count();
    assert!(
        c_progress >= 2,
        "expected >= 2 client progress events, got {c_progress}"
    );

    let s_events = collect_events_until(
        &mut hs.event_rx,
        |e| matches!(e, SessionEvent::TransferComplete { .. }),
        Duration::from_secs(15),
    )
    .await;
    let s_progress = s_events
        .iter()
        .filter(|e| matches!(e, SessionEvent::TransferProgress { .. }))
        .count();
    assert!(
        s_progress >= 2,
        "expected >= 2 server progress events, got {s_progress}"
    );

    let received = tokio::fs::read(recv_dir.path().join("big256k.bin"))
        .await
        .unwrap();
    assert_eq!(received.len(), content.len());
    assert_eq!(received, content);
}

// ---------------------------------------------------------------------------
//  Transfer — empty file
// ---------------------------------------------------------------------------

/// Given an empty file, when transferred, then both sides complete and
/// the empty file is created on disk.
#[tokio::test]
async fn given_empty_file_when_transferred_then_both_sides_complete() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    tokio::fs::write(send_dir.path().join("send_file.txt"), b"")
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request("empty-1", send_dir.path(), "empty.txt", 0);
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "empty-1".into(),
            accept: true,
        })
        .await
        .unwrap();

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;

    let received = tokio::fs::read(recv_dir.path().join("empty.txt"))
        .await
        .unwrap();
    assert!(received.is_empty());
}

// ---------------------------------------------------------------------------
//  Transfer — sequential deny then accept (session reuse)
// ---------------------------------------------------------------------------

/// Given multiple sequential transfers, when the first is denied and
/// the second accepted, then the second file is received correctly.
#[tokio::test]
async fn given_sequential_deny_then_accept_when_same_session_then_second_file_received() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content_a = b"payload A";
    let content_b = b"payload B - different content";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content_a)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    // --- First: offer, deny ---
    let req_a = make_send_request("seq-1", send_dir.path(), "a.txt", content_a.len() as u64);
    hc.cmd_tx
        .send(SessionCmd::Transfer { req: req_a })
        .await
        .unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "seq-1".into(),
            accept: false,
        })
        .await
        .unwrap();

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferDenied { .. })
    })
    .await;

    // --- Second: overwrite file, accept ---
    tokio::fs::write(send_dir.path().join("send_file.txt"), content_b as &[u8])
        .await
        .unwrap();

    let req_b = make_send_request("seq-2", send_dir.path(), "b.txt", content_b.len() as u64);
    hc.cmd_tx
        .send(SessionCmd::Transfer { req: req_b })
        .await
        .unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "seq-2".into(),
            accept: true,
        })
        .await
        .unwrap();

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferComplete { .. })
    })
    .await;

    let received = tokio::fs::read(recv_dir.path().join("b.txt"))
        .await
        .unwrap();
    assert_eq!(received, content_b);
}

// ---------------------------------------------------------------------------
//  Cancellation
// ---------------------------------------------------------------------------

/// Given an idle session, when the client cancels, then Finished is
/// emitted.
#[tokio::test]
async fn given_idle_session_when_cancelled_then_emits_finished() {
    let dir = tempfile::tempdir().unwrap();
    let (mut hc, hs) = mock_session_pair(dir.path(), dir.path());

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    hc.cmd_tx.send(SessionCmd::Cancel).await.unwrap();

    let ev = wait_for_event(&mut hc.event_rx, |e| matches!(e, SessionEvent::Finished)).await;
    assert!(matches!(ev, SessionEvent::Finished));

    let _ = hs.cmd_tx.send(SessionCmd::Cancel).await;
}

/// Given a pending transfer offer, when the client cancels, then both
/// sides terminate.
#[tokio::test]
async fn given_pending_offer_when_client_cancels_then_both_sessions_terminate() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"cancel-me";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request("cancel-1", send_dir.path(), "x.txt", content.len() as u64);
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;

    hc.cmd_tx.send(SessionCmd::Cancel).await.unwrap();

    let ev = wait_for_event(&mut hc.event_rx, |e| matches!(e, SessionEvent::Finished)).await;
    assert!(matches!(ev, SessionEvent::Finished));

    let ev_s = wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Finished | SessionEvent::Error { .. })
    })
    .await;
    assert!(matches!(
        ev_s,
        SessionEvent::Finished | SessionEvent::Error { .. }
    ));
}

// ---------------------------------------------------------------------------
//  Channel drop
// ---------------------------------------------------------------------------

/// Given a client that drops its command channel, when detected, then
/// Finished is emitted.
#[tokio::test]
async fn given_client_when_cmd_channel_dropped_then_session_emits_finished() {
    let dir = tempfile::tempdir().unwrap();
    let (mut hc, _hs) = mock_session_pair(dir.path(), dir.path());

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    drop(hc.cmd_tx);

    let ev = wait_for_event(&mut hc.event_rx, |e| matches!(e, SessionEvent::Finished)).await;
    assert!(matches!(ev, SessionEvent::Finished));
}

/// Given a server that drops its command channel while awaiting a
/// decision, then Finished is emitted.
#[tokio::test]
async fn given_server_when_cmd_channel_dropped_during_offer_then_session_emits_finished() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"orphan offer";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (hc, mut hs) = mock_session_pair(send_dir.path(), recv_dir.path());

    let req = make_send_request("orphan-1", send_dir.path(), "x.txt", content.len() as u64);
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;

    drop(hs.cmd_tx);

    let ev = wait_for_event(&mut hs.event_rx, |e| matches!(e, SessionEvent::Finished)).await;
    assert!(matches!(ev, SessionEvent::Finished));

    let _ = hc.cmd_tx.send(SessionCmd::Cancel).await;
}
