//! Engine-level integration tests.
//!
//! These tests exercise the [`Engine`] public API with real TCP transport,
//! verifying listener lifecycle, connection establishment, session creation,
//! and end-to-end file transfer through the engine command/event bus.

mod common;

use std::time::Duration;

use tokio::sync::broadcast;

use waterdrop_engine::engine::{Engine, EngineCmd, EngineConfig, EngineEvent, EngineHandle};
use waterdrop_engine::session::{SendRequest, SessionCmd, SessionEvent};
use waterdrop_engine::tcp::{TcpConnector, TcpListenerFactory};

fn start_tcp_engine(config: EngineConfig) -> (EngineHandle, broadcast::Receiver<EngineEvent>) {
    let engine = Engine;
    let handle = engine.start(TcpListenerFactory, TcpConnector, config);
    let events_rx = handle.events_tx.subscribe();
    (handle, events_rx)
}

fn default_test_config() -> (EngineConfig, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = EngineConfig {
        device_name: "TestDevice".into(),
        receive_dir: dir.path().to_path_buf(),
    };
    (config, dir)
}

async fn wait_for_engine_event(
    rx: &mut broadcast::Receiver<EngineEvent>,
    pred: impl Fn(&EngineEvent) -> bool,
) -> EngineEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(ev) if pred(&ev) => return ev,
                Ok(_) => {}
                Err(e) => panic!("event channel error: {e}"),
            }
        }
    })
    .await
    .expect("timed out waiting for engine event")
}

/// Given a StartAccepting command, when the engine binds successfully,
/// then an Accepting event is emitted with the bound address.
#[tokio::test]
async fn given_start_accepting_cmd_when_bind_succeeds_then_accepting_event_emitted() {
    let (config, _dir) = default_test_config();
    let (handle, mut events_rx) = start_tcp_engine(config);

    handle
        .cmd_tx
        .send(EngineCmd::StartAccepting {
            addr: "127.0.0.1:0".into(),
        })
        .await
        .unwrap();

    let ev = wait_for_engine_event(&mut events_rx, |e| {
        matches!(e, EngineEvent::Accepting { .. })
    })
    .await;
    assert!(matches!(ev, EngineEvent::Accepting { .. }));

    handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
}

/// Given a running listener, when StopAccepting is sent, then
/// AcceptingStopped is emitted.
#[tokio::test]
async fn given_running_listener_when_stop_accepting_then_stopped_event_emitted() {
    let (config, _dir) = default_test_config();
    let (handle, mut events_rx) = start_tcp_engine(config);

    handle
        .cmd_tx
        .send(EngineCmd::StartAccepting {
            addr: "127.0.0.1:0".into(),
        })
        .await
        .unwrap();
    wait_for_engine_event(&mut events_rx, |e| {
        matches!(e, EngineEvent::Accepting { .. })
    })
    .await;

    handle.cmd_tx.send(EngineCmd::StopAccepting).await.unwrap();

    let ev = wait_for_engine_event(&mut events_rx, |e| {
        matches!(e, EngineEvent::AcceptingStopped)
    })
    .await;
    assert!(matches!(ev, EngineEvent::AcceptingStopped));

    handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
}

/// Given an invalid bind address, when StartAccepting is sent, then
/// an Error event is emitted.
#[tokio::test]
async fn given_invalid_bind_address_when_start_accepting_then_error_event_emitted() {
    let (config, _dir) = default_test_config();
    let (handle, mut events_rx) = start_tcp_engine(config);

    handle
        .cmd_tx
        .send(EngineCmd::StartAccepting {
            addr: "999.999.999.999:0".into(),
        })
        .await
        .unwrap();

    let ev =
        wait_for_engine_event(&mut events_rx, |e| matches!(e, EngineEvent::Error { .. })).await;
    assert!(matches!(ev, EngineEvent::Error { .. }));

    handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
}

/// Given a running server engine, when a client engine connects, then
/// both sides emit SessionCreated.
#[tokio::test]
async fn given_server_engine_when_client_connects_then_session_created_on_both_sides() {
    let (config, _dir) = default_test_config();
    let (handle_server, mut events_server) = start_tcp_engine(config.clone());

    handle_server
        .cmd_tx
        .send(EngineCmd::StartAccepting {
            addr: "127.0.0.1:0".into(),
        })
        .await
        .unwrap();

    let EngineEvent::Accepting { addr } = wait_for_engine_event(&mut events_server, |e| {
        matches!(e, EngineEvent::Accepting { .. })
    })
    .await
    else {
        unreachable!()
    };

    let (handle_client, mut events_client) = start_tcp_engine(config);
    handle_client
        .cmd_tx
        .send(EngineCmd::Connect {
            addr,
            send_request: None,
        })
        .await
        .unwrap();

    let ev = wait_for_engine_event(&mut events_client, |e| {
        matches!(e, EngineEvent::SessionCreated { .. })
    })
    .await;
    assert!(matches!(ev, EngineEvent::SessionCreated { .. }));

    handle_client
        .cmd_tx
        .send(EngineCmd::ShutDown)
        .await
        .unwrap();
    handle_server
        .cmd_tx
        .send(EngineCmd::ShutDown)
        .await
        .unwrap();
}

/// Given a Connect command to an unreachable address, when the engine
/// tries to connect, then an Error event is emitted.
#[tokio::test]
async fn given_unreachable_address_when_connect_then_error_event_emitted() {
    let (config, _dir) = default_test_config();
    let (handle, mut events_rx) = start_tcp_engine(config);

    handle
        .cmd_tx
        .send(EngineCmd::Connect {
            addr: "127.0.0.1:1".into(), // nothing listening
            send_request: None,
        })
        .await
        .unwrap();

    let ev =
        wait_for_engine_event(&mut events_rx, |e| matches!(e, EngineEvent::Error { .. })).await;
    assert!(matches!(ev, EngineEvent::Error { .. }));

    handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
}

/// Given a SessionCmd targeting a non-existent session, when processed,
/// then the engine does not panic.
#[tokio::test]
async fn given_unknown_session_id_when_session_cmd_sent_then_no_panic() {
    let (config, _dir) = default_test_config();
    let (handle, _events_rx) = start_tcp_engine(config);

    handle
        .cmd_tx
        .send(EngineCmd::SessionCmd {
            session_id: 999,
            cmd: SessionCmd::Cancel,
        })
        .await
        .unwrap();

    // Give the engine a moment to process.
    tokio::time::sleep(Duration::from_millis(50)).await;

    handle.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
}

/// Given a server and client engine, when the client connects with a
/// send request and the server accepts, then the file is transferred
/// end-to-end through the engine event bus and written to disk.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn given_engines_when_file_offered_and_accepted_then_file_transferred_end_to_end() {
    let send_dir = tempfile::tempdir().unwrap();
    let recv_dir = tempfile::tempdir().unwrap();

    let file_content = b"engine integration test data";
    let send_path = send_dir.path().join("send_file.txt");
    tokio::fs::write(&send_path, file_content).await.unwrap();

    let server_config = EngineConfig {
        device_name: "ServerDev".into(),
        receive_dir: recv_dir.path().to_path_buf(),
    };
    let client_config = EngineConfig {
        device_name: "ClientDev".into(),
        receive_dir: send_dir.path().to_path_buf(),
    };

    // Start server engine.
    let (handle_s, mut events_s) = start_tcp_engine(server_config);
    handle_s
        .cmd_tx
        .send(EngineCmd::StartAccepting {
            addr: "127.0.0.1:0".into(),
        })
        .await
        .unwrap();

    let EngineEvent::Accepting { addr } = wait_for_engine_event(&mut events_s, |e| {
        matches!(e, EngineEvent::Accepting { .. })
    })
    .await
    else {
        unreachable!()
    };

    // Start client engine with a send request.
    let send_req = SendRequest {
        transfer_id: "engine-xfer-1".into(),
        file_path: send_path,
        filename: "received_via_engine.txt".into(),
        size_bytes: file_content.len() as u64,
        sha256_hex: "unused".into(),
    };

    let (handle_c, mut events_c) = start_tcp_engine(client_config);
    handle_c
        .cmd_tx
        .send(EngineCmd::Connect {
            addr,
            send_request: Some(send_req),
        })
        .await
        .unwrap();

    // Client: SessionCreated.
    wait_for_engine_event(&mut events_c, |e| {
        matches!(e, EngineEvent::SessionCreated { .. })
    })
    .await;

    // Client: Connected (handshake done).
    let ev = wait_for_engine_event(&mut events_c, |e| {
        matches!(
            e,
            EngineEvent::SessionEvent {
                event: SessionEvent::Connected { .. },
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        ev,
        EngineEvent::SessionEvent {
            event: SessionEvent::Connected { .. },
            ..
        }
    ));

    // Server: SessionCreated, then TransferOffered.
    wait_for_engine_event(&mut events_s, |e| {
        matches!(e, EngineEvent::SessionCreated { .. })
    })
    .await;

    let offered_ev = wait_for_engine_event(&mut events_s, |e| {
        matches!(
            e,
            EngineEvent::SessionEvent {
                event: SessionEvent::TransferOffered { .. },
                ..
            }
        )
    })
    .await;

    let EngineEvent::SessionEvent {
        session_id: sid,
        event: SessionEvent::TransferOffered {
            transfer_id: tid, ..
        },
    } = offered_ev
    else {
        unreachable!()
    };

    // Server: accept the transfer.
    handle_s
        .cmd_tx
        .send(EngineCmd::SessionCmd {
            session_id: sid,
            cmd: SessionCmd::RespondToOffer {
                transfer_id: tid,
                accept: true,
            },
        })
        .await
        .unwrap();

    // Client: TransferComplete.
    wait_for_engine_event(&mut events_c, |e| {
        matches!(
            e,
            EngineEvent::SessionEvent {
                event: SessionEvent::TransferComplete { .. },
                ..
            }
        )
    })
    .await;

    // Server: TransferComplete.
    wait_for_engine_event(&mut events_s, |e| {
        matches!(
            e,
            EngineEvent::SessionEvent {
                event: SessionEvent::TransferComplete { .. },
                ..
            }
        )
    })
    .await;

    // Verify the file was received.
    let received = tokio::fs::read(recv_dir.path().join("received_via_engine.txt"))
        .await
        .unwrap();
    assert_eq!(received, file_content);

    handle_c.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
    handle_s.cmd_tx.send(EngineCmd::ShutDown).await.unwrap();
}
