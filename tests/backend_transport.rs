use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use codex_acp_v2::backend::{Backend, BackendError, BackendEvent, BackendOptions};
use serde_json::json;
use tokio::sync::mpsc;

fn options(scenario: &str) -> BackendOptions {
    BackendOptions {
        executable: PathBuf::from(if cfg!(windows) { "python" } else { "python3" }),
        args: vec![
            OsString::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/codex_peer.py"
            )),
            OsString::from(scenario),
        ],
        request_timeout: Duration::from_secs(5),
        max_in_flight: 1,
        ..BackendOptions::default()
    }
}

async fn event(events: &mut mpsc::Receiver<BackendEvent>) -> BackendEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn responses_and_callbacks_remain_responsive_and_cancelled_requests_release_capacity() {
    let (backend, mut events) = Backend::spawn(options("transport")).await.unwrap();
    // Deliberately leave the activity stream unread while awaiting a response.
    assert_eq!(
        backend.request("exercise", json!({})).await.unwrap(),
        json!({"accepted": true})
    );
    assert!(
        matches!(event(&mut events).await, BackendEvent::Notification { method, .. } if method == "thread/started")
    );
    let BackendEvent::ServerRequest { id, method, .. } = event(&mut events).await else {
        panic!("expected approval")
    };
    assert_eq!(method, "item/commandExecution/requestApproval");
    backend
        .respond(id, Ok(json!({"decision": "decline"})))
        .await
        .unwrap();
    assert!(
        matches!(event(&mut events).await, BackendEvent::Notification { method, params, .. } if method == "approval-received" && params == json!({"decision": "decline"}))
    );

    let pending_backend = backend.clone();
    let pending =
        tokio::spawn(async move { pending_backend.request("cancel-me", json!({})).await });
    assert!(
        matches!(event(&mut events).await, BackendEvent::Notification { method, .. } if method == "waiting")
    );
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert_eq!(
        backend.request("after-cancel", json!({})).await.unwrap(),
        json!({"stillResponsive": true})
    );

    let BackendError::Rpc(error) = backend.request("rpc-error", json!({})).await.unwrap_err()
    else {
        panic!("expected structured RPC error")
    };
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        json!({"code": -32001, "message": "policy", "data": {"scope": "turn"}})
    );
    backend.shutdown().await.unwrap();
    assert!(matches!(
        event(&mut events).await,
        BackendEvent::Disconnected { .. }
    ));
}

#[tokio::test]
async fn resource_exhaustion_explicitly_disconnects_and_resolves_pending_requests() {
    for scenario in ["overflow", "oversize"] {
        let (backend, mut events) = Backend::spawn(BackendOptions {
            event_capacity: 2,
            max_frame_bytes: 1024,
            ..options(scenario)
        })
        .await
        .unwrap();
        let error = backend.request("trigger", json!({})).await.unwrap_err();
        assert!(
            matches!(error, BackendError::Disconnected(_)),
            "{scenario}: {error}"
        );
        // Even when the activity queue fills, its reserved terminal slot survives.
        let message = loop {
            if let BackendEvent::Disconnected { message } = event(&mut events).await {
                break message;
            }
        };
        assert!(
            message.contains(if scenario == "overflow" {
                "queue overflow"
            } else {
                "max_frame_bytes"
            }),
            "{message}"
        );
        backend.shutdown().await.unwrap();
    }
}
