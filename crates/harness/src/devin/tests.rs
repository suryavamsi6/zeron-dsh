//! The Devin harness against a node shim speaking ACP over stdio
//! (`tests/devin_acp_shim.mjs`): end-to-end turn, steering preemption,
//! interrupt, permission auto-accept and the auth failure surface. The shims
//! are unix-only (the launcher is a shell script); the pure mapping tests
//! run everywhere.

use super::*;
use std::path::Path;
use std::sync::Arc;

use zeron_proto::RunRequest;

fn base_request(dir: &Path) -> RunRequest {
    serde_json::from_value(serde_json::json!({
        "prompt": "first", "model": null, "reasoning": null, "cwd": dir,
        "sandbox": "workspace-write", "resume": null, "access": "full-access"
    }))
    .unwrap()
}

fn shim_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// A shell launcher for the node shim, executable on PATH-free lookup via
/// the harness's test override.
fn write_launcher(dir: &Path, env_lines: &str) -> PathBuf {
    let shim = shim_dir().join("devin_acp_shim.mjs");
    let launcher = dir.join("devin-shim");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\n{env_lines}\nexec node '{}' \"$@\"\n",
            shim.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    launcher
}

fn controls() -> (
    RunControls,
    mpsc::Sender<crate::SteerMessage>,
    CancellationToken,
) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let interrupt = CancellationToken::new();
    let controls = RunControls {
        execution_lease: None,
        request_input: Box::new(|_| {
            let (_tx, rx) = tokio::sync::oneshot::channel();
            rx
        }),
        steering: steer_rx,
        interrupt: interrupt.clone(),
        turn: crate::TurnControl::default(),
    };
    (controls, steer_tx, interrupt)
}

use tokio::sync::mpsc;
use crate::CancellationToken;

async fn collect(
    mut stream: BoxStream<'static, Result<AgentEvent, HarnessError>>,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("stream error");
        let done = matches!(event, AgentEvent::Done { .. });
        events.push(event);
        if done {
            break;
        }
    }
    events
}

#[tokio::test]
#[cfg(unix)]
async fn a_turn_streams_text_and_ends_completed() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = write_launcher(dir.path(), "");
    let mut harness = DevinHarness::new();
    harness.test_executable = Some(launcher);
    let (controls, _steer, _interrupt) = controls();
    let stream = harness.run(base_request(dir.path()), controls).await.unwrap();
    let events = collect(stream).await;
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
        "no SessionStarted in {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("echo: first"))),
        "no echo TextDelta in {events:?}"
    );
    let done = events.iter().find_map(|e| match e {
        AgentEvent::Done { status, .. } => Some(*status),
        _ => None,
    });
    assert_eq!(done, Some(DoneStatus::Completed), "events: {events:?}");
}

#[tokio::test]
#[cfg(unix)]
async fn an_auth_required_handshake_error_points_at_devin_auth_login() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = write_launcher(dir.path(), "export SHIM_AUTH_REQUIRED=1");
    let mut harness = DevinHarness::new();
    harness.test_executable = Some(launcher);
    let (controls, _steer, _interrupt) = controls();
    let stream = harness.run(base_request(dir.path()), controls).await.unwrap();
    let events = collect(stream).await;
    let error = events.iter().find_map(|e| match e {
        AgentEvent::Done { error, .. } => error.clone(),
        _ => None,
    });
    assert!(
        error.as_deref().is_some_and(|e| e.contains("devin auth login")),
        "error: {error:?}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn a_permission_request_is_auto_accepted_with_the_preferred_allow() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("shim.log");
    let launcher = write_launcher(
        dir.path(),
        &format!(
            "export SHIM_PERMISSION=1\nexport SHIM_LOG='{}'",
            log.display()
        ),
    );
    let mut harness = DevinHarness::new();
    harness.test_executable = Some(launcher);
    let (controls, _steer, _interrupt) = controls();
    let stream = harness.run(base_request(dir.path()), controls).await.unwrap();
    let events = collect(stream).await;
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Done { status: DoneStatus::Completed, .. })), "{events:?}");
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(
        log.contains("permission-answer") && log.contains("allow-once-id"),
        "log: {log}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn a_mid_turn_steer_cancels_and_reprompts() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("shim.log");
    // Hold the first prompt's response so the steer lands mid-turn.
    let launcher = write_launcher(
        dir.path(),
        &format!(
            "export SHIM_PROMPT_DELAY_MS=3000\nexport SHIM_LOG='{}'",
            log.display()
        ),
    );
    let mut harness = DevinHarness::new();
    harness.test_executable = Some(launcher);
    let (controls, steer_tx, _interrupt) = controls();
    let stream = harness.run(base_request(dir.path()), controls).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    steer_tx
        .send(crate::SteerMessage {
            prompt: "steer now".into(),
            message_id: None,
            attachments: Vec::new(),
            config: None,
        })
        .await
        .unwrap();
    let events = collect(stream).await;
    // The shim does not implement cancel-settling (it answers the held
    // prompt on its timer regardless); what proves the preemption is the
    // wire: a cancel notification, then a second prompt carrying the steer.
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains("session/cancel"), "no cancel in log: {log}");
    assert!(log.contains("steer now"), "no steer reprompt in log: {log}");
    drop(events);
}

#[tokio::test]
#[cfg(unix)]
async fn an_interrupt_ends_the_run_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = write_launcher(dir.path(), "export SHIM_PROMPT_DELAY_MS=10000");
    let mut harness = DevinHarness::new();
    harness.test_executable = Some(launcher);
    let (controls, _steer, interrupt) = controls();
    let stream = harness.run(base_request(dir.path()), controls).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    interrupt.cancel();
    let events = collect(stream).await;
    let done = events.iter().find_map(|e| match e {
        AgentEvent::Done { status, .. } => Some(*status),
        _ => None,
    });
    assert_eq!(done, Some(DoneStatus::Interrupted), "events: {events:?}");
}

#[test]
fn stop_reasons_map_to_done_statuses() {
    assert_eq!(
        stop_reason_status(&serde_json::json!({ "stopReason": "end_turn" })),
        DoneStatus::Completed
    );
    assert_eq!(
        stop_reason_status(&serde_json::json!({ "stopReason": "cancelled" })),
        DoneStatus::Interrupted
    );
    assert_eq!(
        stop_reason_status(&serde_json::json!({ "stopReason": "refusal" })),
        DoneStatus::Errored
    );
}

#[test]
fn auth_failures_are_recognized() {
    assert!(is_auth_required(&HarnessError::Protocol(
        "session/new: auth_required: sign in first".into()
    )));
    assert!(!is_auth_required(&HarnessError::Protocol(
        "session/new: no such session".into()
    )));
}

#[test]
fn devin_is_installed_only_with_a_binary() {
    let harness = DevinHarness::new();
    // The CI box has no devin CLI; the override seam is what the shim tests use.
    assert_eq!(harness.installed(), resolve_executable().is_some());
}

#[test]
fn the_zcron_mcp_server_rides_session_new() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = base_request(dir.path());
    request.mcp = Some(serde_json::from_value(serde_json::json!({
        "name": "zeron",
        "command": "/bin/zeron",
        "args": ["mcp"],
        "env": {"ZERO_ORG": "x"}
    }))
    .unwrap());
    let servers = session_mcp_servers(&request);
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["name"], "zeron");
    assert_eq!(servers[0]["env"][0]["name"], "ZERO_ORG");
}

#[allow(dead_code)]
fn unused(_: Arc<()>) {}
