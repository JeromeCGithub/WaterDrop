//! Session functional tests over real QUIC transport.
//!
//! These tests exercise the full [`Session`] state machine over actual
//! QUIC connections (loopback) to catch transport-level issues such as
//! the `CONNECTION_CLOSE` race, stream delivery ordering, and graceful
//! shutdown behaviour that cannot be reproduced with in-memory mocks.

mod common;

use std::time::Duration;

use common::{collect_events_until, make_send_request, quic_session_pair, wait_for_event};
use waterdrop_engine::session::{SessionCmd, SessionEvent};

/// Given a real QUIC connection, when the handshake completes, then
/// both sides report the correct peer device name.
#[tokio::test]
async fn given_quic_connection_when_handshake_completes_then_peer_names_are_correct() {
    let dir = tempfile::tempdir().unwrap();

    let (mut hc, mut hs) = quic_session_pair(dir.path(), dir.path()).await;

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
    assert_eq!(client_sees, "QuicServer");

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
    assert_eq!(server_sees, "QuicClient");

    let _ = hc.cmd_tx.send(SessionCmd::Cancel).await;
    let _ = hs.cmd_tx.send(SessionCmd::Cancel).await;
}

/// Given a real QUIC connection, when a file is transferred and
/// accepted, then both sides emit TransferComplete and Finished
/// without any errors (no CONNECTION_CLOSE race).
#[tokio::test]
async fn given_quic_transfer_when_accepted_then_both_sides_complete_without_error() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"QUIC integration test payload - no race!";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = quic_session_pair(send_dir.path(), recv_dir.path()).await;

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    let req = make_send_request(
        "quic-xfer-1",
        send_dir.path(),
        "received.txt",
        content.len() as u64,
    );
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "quic-xfer-1".into(),
            accept: true,
        })
        .await
        .unwrap();

    // Client: collect all events until Finished.
    let client_events = collect_events_until(
        &mut hc.event_rx,
        |e| matches!(e, SessionEvent::Finished),
        Duration::from_secs(10),
    )
    .await;

    assert!(
        client_events
            .iter()
            .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
        "client must see TransferComplete, got: {client_events:?}"
    );
    assert!(
        client_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Finished)),
        "client must see Finished, got: {client_events:?}"
    );
    assert!(
        !client_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Error { .. })),
        "client must NOT see Error, got: {client_events:?}"
    );

    // Server: collect all events until Finished.
    let server_events = collect_events_until(
        &mut hs.event_rx,
        |e| matches!(e, SessionEvent::Finished),
        Duration::from_secs(10),
    )
    .await;

    assert!(
        server_events
            .iter()
            .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
        "server must see TransferComplete, got: {server_events:?}"
    );
    assert!(
        !server_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Error { .. })),
        "server must NOT see Error, got: {server_events:?}"
    );

    // Verify file content.
    let received = tokio::fs::read(recv_dir.path().join("received.txt"))
        .await
        .unwrap();
    assert_eq!(received, content);
}

/// Given a real QUIC connection, when a 256 KB file is transferred,
/// then progress events are emitted on both sides and data arrives
/// intact.
#[tokio::test]
async fn given_quic_large_file_when_transferred_then_progress_emitted_and_data_intact() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    #[allow(clippy::cast_possible_truncation)]
    let content: Vec<u8> = (0..262_144u32).map(|i| (i % 251) as u8).collect();
    tokio::fs::write(send_dir.path().join("send_file.txt"), &content)
        .await
        .unwrap();

    let (mut hc, mut hs) = quic_session_pair(send_dir.path(), recv_dir.path()).await;

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    let req = make_send_request("quic-big", send_dir.path(), "big.bin", content.len() as u64);
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "quic-big".into(),
            accept: true,
        })
        .await
        .unwrap();

    // Client events.
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
    assert!(
        !c_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Error { .. })),
        "client must NOT see Error, got: {c_events:?}"
    );

    // Server events.
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

    // Verify data integrity.
    let received = tokio::fs::read(recv_dir.path().join("big.bin"))
        .await
        .unwrap();
    assert_eq!(received.len(), content.len());
    assert_eq!(received, content);
}

/// Given a real QUIC connection, when the server denies the transfer,
/// then the client sees TransferDenied and both sides remain
/// operational.
#[tokio::test]
async fn given_quic_transfer_when_denied_then_client_sees_denied_without_error() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"deny-me over QUIC";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = quic_session_pair(send_dir.path(), recv_dir.path()).await;

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    let req = make_send_request(
        "quic-deny",
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
            transfer_id: "quic-deny".into(),
            accept: false,
        })
        .await
        .unwrap();

    let ev = wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::TransferDenied { .. })
    })
    .await;
    assert!(
        matches!(ev, SessionEvent::TransferDenied { transfer_id } if transfer_id == "quic-deny")
    );

    let _ = hc.cmd_tx.send(SessionCmd::Cancel).await;
    let _ = hs.cmd_tx.send(SessionCmd::Cancel).await;
}

/// Given a real QUIC connection with an empty file, when transferred,
/// then both sides complete and the empty file is created.
#[tokio::test]
async fn given_quic_empty_file_when_transferred_then_both_sides_complete() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    tokio::fs::write(send_dir.path().join("send_file.txt"), b"")
        .await
        .unwrap();

    let (mut hc, mut hs) = quic_session_pair(send_dir.path(), recv_dir.path()).await;

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    let req = make_send_request("quic-empty", send_dir.path(), "empty.txt", 0);
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;
    hs.cmd_tx
        .send(SessionCmd::RespondToOffer {
            transfer_id: "quic-empty".into(),
            accept: true,
        })
        .await
        .unwrap();

    // Both complete without error.
    let c_events = collect_events_until(
        &mut hc.event_rx,
        |e| matches!(e, SessionEvent::Finished),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        c_events
            .iter()
            .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
        "client must see TransferComplete"
    );
    assert!(
        !c_events
            .iter()
            .any(|e| matches!(e, SessionEvent::Error { .. })),
        "client must NOT see Error, got: {c_events:?}"
    );

    let s_events = collect_events_until(
        &mut hs.event_rx,
        |e| matches!(e, SessionEvent::Finished),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        s_events
            .iter()
            .any(|e| matches!(e, SessionEvent::TransferComplete { .. })),
        "server must see TransferComplete"
    );

    let received = tokio::fs::read(recv_dir.path().join("empty.txt"))
        .await
        .unwrap();
    assert!(received.is_empty());
}

/// Given a real QUIC connection, when the client cancels mid-session,
/// then both sides terminate without panicking.
#[tokio::test]
async fn given_quic_session_when_client_cancels_then_both_sides_terminate() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let content = b"cancel me over QUIC";
    tokio::fs::write(send_dir.path().join("send_file.txt"), content)
        .await
        .unwrap();

    let (mut hc, mut hs) = quic_session_pair(send_dir.path(), recv_dir.path()).await;

    wait_for_event(&mut hc.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;
    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Connected { .. })
    })
    .await;

    let req = make_send_request(
        "quic-cancel",
        send_dir.path(),
        "x.txt",
        content.len() as u64,
    );
    hc.cmd_tx.send(SessionCmd::Transfer { req }).await.unwrap();

    wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::TransferOffered { .. })
    })
    .await;

    // Cancel client before server responds.
    hc.cmd_tx.send(SessionCmd::Cancel).await.unwrap();

    let ev_c = wait_for_event(&mut hc.event_rx, |e| matches!(e, SessionEvent::Finished)).await;
    assert!(matches!(ev_c, SessionEvent::Finished));

    // Server should also terminate.
    let ev_s = wait_for_event(&mut hs.event_rx, |e| {
        matches!(e, SessionEvent::Finished | SessionEvent::Error { .. })
    })
    .await;
    assert!(matches!(
        ev_s,
        SessionEvent::Finished | SessionEvent::Error { .. }
    ));
}
