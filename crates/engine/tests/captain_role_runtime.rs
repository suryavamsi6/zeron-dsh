//! A Captain role change must reach a PARKED runtime: promote or demote a chat
//! whose turn completed and whose runtime stayed warm, and the next send must
//! launch a fresh runtime re-prepared under the new roster — the parked child
//! was spawned before the roster moved and still holds the role it started
//! with.
//!
//! On current main this does not happen: `dispatch_inner` decides warm vs
//! fresh from the stored run request (`route.is_some()`), and the roster stamp
//! is only inserted into `request.env` later, inside `drive_run`'s `prepare`
//! (`PinRunEnvironment`, agent_accounts.rs) — so a send after a role change
//! takes the steer path and no `run()` is ever recorded for it.
//!
//! The harness therefore reports `HarnessId::DeepSeek`: only DeepSeek runs get
//! the `ZERON_DEEPSEEK_CAPTAINS` stamp (Mock runs prepare as a no-op), and the
//! stamp is what the assertions read. `same_runtime` mirrors the real driver,
//! env included, so a stamped-vs-unstamped request can never route as the same
//! runtime.
//!
//! The control send matters: with no roster change the second send MUST take
//! the warm path (no new `RunRequest`) — reuse itself is correct; reuse across
//! a role change is the defect.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionCommandPayload};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-captain-role";
const DEVICE: &str = "dev-captains";
const CAPTAINS_ENV: &str = "ZERON_DEEPSEEK_CAPTAINS";

/// One persistent agent process per `run()`: answers the launch prompt, then
/// parks warm and answers every steer the mailbox delivers. Reports DeepSeek
/// so `PinRunEnvironment` stamps the run with the chat's Captain role.
struct Agent {
    runs: Arc<Mutex<Vec<RunRequest>>>,
}

fn done(status: DoneStatus) -> AgentEvent {
    AgentEvent::Done {
        status,
        result: None,
        error: None,
        session_id: Some("hs".into()),
    }
}

#[async_trait]
impl Harness for Agent {
    fn id(&self) -> HarnessId {
        HarnessId::DeepSeek
    }
    fn display_name(&self) -> &str {
        "Agent"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    /// A turn ends exactly on its wire `Done` — no quiesce backstop.
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    /// The real driver's rule (harness/deepseek): a launch-affecting
    /// difference — `env` among them — can never ride the warm path.
    fn same_runtime(&self, live: &RunRequest, next: &RunRequest) -> bool {
        live.model == next.model
            && live.reasoning == next.reasoning
            && zeron_harness::same_model_options(&live.model_options, &next.model_options)
            && live.cwd == next.cwd
            && live.access == next.access
            && live.plan == next.plan
            && live.sandbox == next.sandbox
            && live.mcp == next.mcp
            && live.extra_mcp == next.extra_mcp
            && live.env == next.env
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        mut controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.runs.lock().unwrap().push(request.clone());
        let (tx, rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(64);
        tokio::spawn(async move {
            let send = |event: AgentEvent| {
                let tx = tx.clone();
                async move { tx.send(Ok(event)).await.is_ok() }
            };
            let _ = send(AgentEvent::SessionStarted {
                harness: HarnessId::DeepSeek,
                model: "mock-deepseek".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "hs".into(),
                assistant_message_id: "a-1".into(),
            })
            .await;
            if !send(AgentEvent::TextDelta {
                text: format!("answered: {}", request.prompt),
            })
            .await
                || !send(done(DoneStatus::Completed)).await
            {
                return;
            }
            // Parked between turns: each mailbox steer is a new turn.
            loop {
                tokio::select! {
                    biased;
                    _ = controls.interrupt.cancelled() => {
                        let _ = send(done(DoneStatus::Interrupted)).await;
                        return;
                    }
                    steer = controls.steering.recv() => {
                        let Some(steer) = steer else { return };
                        let boundary = AgentEvent::Steered {
                            assistant_message_id: None,
                            next_assistant_message_id: None,
                        };
                        if !send(boundary).await
                            || !send(AgentEvent::TextDelta {
                                text: format!("answered: {}", steer.prompt),
                            }).await
                            || !send(done(DoneStatus::Completed)).await
                        {
                            return;
                        }
                    }
                }
            }
        });
        Ok(
            futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|e| (e, rx))
            })
            .boxed(),
        )
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    dir: std::path::PathBuf,
    runs: Arc<Mutex<Vec<RunRequest>>>,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("device-id"), DEVICE).unwrap();
        // The roster the engine's CaptainsStore opens at assembly — empty;
        // only the roster RPCs write rows after this.
        std::fs::write(dir.join("captains.json"), r#"{"captains":[]}"#).unwrap();
        Self {
            _tmp: tmp,
            dir,
            runs: Arc::default(),
        }
    }

    /// One start of the engine on this machine's disk — the test boots once.
    fn boot(&self) -> EngineCore {
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(Agent {
            runs: self.runs.clone(),
        }));
        let core = EngineCore::assemble(&self.dir, Arc::new(registry), HarnessId::DeepSeek, None)
            .expect("engine core assembles");
        core.sessions.set_no_ipc_port();
        core
    }

    fn recorded(&self) -> std::sync::MutexGuard<'_, Vec<RunRequest>> {
        self.runs.lock().unwrap()
    }
}

async fn create_chat(core: &EngineCore) {
    let client = zeron_rpc::memory_client(core.rpc_service());
    let _ = client
        .call(
            zeron_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": CHAT, "deviceId": core.device_id }),
        )
        .await;
    let _ = core.workspace.rename_chat(CHAT, "Pre-titled");
}

fn send(core: &EngineCore, id: &str, prompt: &str) {
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: RunRequest {
                    mcp: None,
                    prompt: prompt.into(),
                    harness: Some(HarnessId::DeepSeek),
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "~".into(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: true,
                    access: Default::default(),
                    plan: false,
                    attachments: Vec::new(),
                    worktree: None,
                    resume: None,
                    env: Default::default(),
                    extra_mcp: Vec::new(),
                },
                message_id: id.into(),
            },
        )
        .expect("queue run command");
}

/// The reply for `prompt` folded complete into the doc.
fn answered(core: &EngineCore, prompt: &str) -> bool {
    let text = format!("answered: {prompt}");
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .any(|e| {
            e.role == MessageRole::Assistant
                && e.status == Some(MessageStatus::Complete)
                && e.parts
                    .iter()
                    .any(|p| matches!(p, MessagePart::Text { text: t, .. } if t == &text))
        })
}

/// The turn is over and the runtime sits warm, mailbox open — not in flight.
fn parked(core: &EngineCore) -> bool {
    core.sessions
        .session_status(CHAT)
        .is_some_and(|s| s.status == SessionStatus::Idle)
}

async fn wait_for(mut what: impl FnMut() -> bool, label: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !what() {
        assert!(std::time::Instant::now() < deadline, "timed out: {label}");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// Poll `Captains` until the roster reads `found`, same bounded style as
/// `wait_for` — the mutation RPC's own reply is only the write's echo.
async fn wait_roster(
    client: &zeron_rpc::RpcClient,
    label: &str,
    found: impl Fn(&serde_json::Value) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let roster = client
            .call(zeron_rpc::methods::CAPTAINS, serde_json::json!({}))
            .await
            .expect("the Captains roster reads");
        if found(&roster) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out: {label}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_captain_role_change_reaches_a_parked_runtime() {
    let rig = Rig::new();
    let core = rig.boot();
    create_chat(&core).await;
    let client = zeron_rpc::memory_client(core.rpc_service());

    // (1) An ordinary turn: one `run()`, and it carried no Captain stamp.
    send(&core, "m1", "hello");
    wait_for(
        || answered(&core, "hello") && parked(&core),
        "the first turn",
    )
    .await;
    {
        let runs = rig.recorded();
        assert_eq!(
            runs.len(),
            1,
            "expected the first turn to record exactly one RunRequest; saw {}",
            runs.len()
        );
        assert!(
            !runs[0].env.contains_key(CAPTAINS_ENV),
            "an ordinary chat's run must carry no {CAPTAINS_ENV} stamp; saw env keys {:?}",
            runs[0].env.keys().collect::<Vec<_>>()
        );
    }

    // (4) Control — no roster change between sends: the second send steers
    // into the parked runtime, so nothing new is recorded. Reuse here is
    // correct; it is the SAME reuse the promote below must not get.
    send(&core, "m2", "still there");
    wait_for(
        || answered(&core, "still there") && parked(&core),
        "the control turn",
    )
    .await;
    {
        let runs = rig.recorded();
        assert_eq!(
            runs.len(),
            1,
            "with no role change the parked runtime is reused: expected still \
             1 recorded RunRequest, saw {}",
            runs.len()
        );
    }

    // (2) Promote reaches the parked runtime: a fresh `run()` carries the
    // stamp of the role the chat now plays.
    client
        .call(
            zeron_rpc::methods::PROMOTE_CAPTAIN,
            serde_json::json!({ "chatId": CHAT, "name": "Helm" }),
        )
        .await
        .expect("PromoteCaptain");
    wait_roster(&client, "the promote readback", |roster| {
        roster["captains"].as_array().is_some_and(|captains| {
            captains.iter().any(|c| {
                c["chatId"] == CHAT && c["name"] == "Helm" && c["archived"] == false
            })
        })
    })
    .await;
    send(&core, "m3", "after the promote");
    wait_for(
        || answered(&core, "after the promote") && parked(&core),
        "the post-promote turn",
    )
    .await;
    {
        let runs = rig.recorded();
        assert_eq!(
            runs.len(),
            2,
            "promoting {CHAT} must replace the parked runtime: expected a \
             second recorded RunRequest, saw {} — the parked runtime was \
             reused and the role change was never applied",
            runs.len()
        );
        let stamp = runs[1]
            .env
            .get(CAPTAINS_ENV)
            .unwrap_or_else(|| panic!("a Captain's run must carry the {CAPTAINS_ENV} stamp; saw env keys {:?}", runs[1].env.keys().collect::<Vec<_>>()));
        let stamp: serde_json::Value =
            serde_json::from_str(stamp).expect("the captains stamp parses as JSON");
        assert_eq!(
            stamp["role"], "captain",
            "the promoted chat stamps as its Captain; saw {stamp}"
        );
        assert_eq!(
            stamp["chatId"], CHAT,
            "the stamp names this chat; saw {stamp}"
        );
        assert_eq!(
            stamp["captainId"], CHAT,
            "a Captain's own id heads its stamp; saw {stamp}"
        );
        assert_eq!(
            stamp["captainName"], "Helm",
            "the promoted name heads the stamp; saw {stamp}"
        );
        let drive = stamp["driveRoot"].as_str().unwrap_or_default();
        assert!(
            drive.ends_with(&format!("/captains/{CHAT}")),
            "driveRoot must end in /captains/{CHAT}; saw {drive}"
        );
    }

    // (3) Demote reaches the parked runtime too: the next `run()` goes back
    // to unstamped.
    client
        .call(
            zeron_rpc::methods::DEMOTE_CAPTAIN,
            serde_json::json!({ "chatId": CHAT }),
        )
        .await
        .expect("DemoteCaptain");
    wait_roster(&client, "the demote readback", |roster| {
        roster["captains"].as_array().is_some_and(|captains| {
            captains
                .iter()
                .any(|c| c["chatId"] == CHAT && c["archived"] == true)
        })
    })
    .await;
    send(&core, "m4", "after the demote");
    wait_for(
        || answered(&core, "after the demote") && parked(&core),
        "the post-demote turn",
    )
    .await;
    {
        let runs = rig.recorded();
        assert_eq!(
            runs.len(),
            3,
            "demoting {CHAT} must replace the parked runtime: expected a \
             third recorded RunRequest, saw {} — the parked runtime was \
             reused and the role change was never applied",
            runs.len()
        );
        // `captains_env` (captains.rs) returns `None` for an ordinary chat —
        // the engine leaves the key unset rather than stamping {"role":"none"}.
        assert!(
            !runs[2].env.contains_key(CAPTAINS_ENV),
            "a demoted chat's run must carry no {CAPTAINS_ENV} stamp; saw env keys {:?}",
            runs[2].env.keys().collect::<Vec<_>>()
        );
    }

    core.shutdown().await;
}
