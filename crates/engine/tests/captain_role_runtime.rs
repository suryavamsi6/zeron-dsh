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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, watch};

use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionCommandPayload};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-captain-role";
const CHAT_PROMOTE: &str = "chat-sur201-promote";
const CHAT_DEMOTE: &str = "chat-sur201-demote";
const CHAT_SIDEBAR: &str = "chat-sur201-sidebar";
const CAP: &str = "chat-sur101-cap";
const CREW: &str = "chat-sur101-crew";
const CREW2: &str = "chat-sur101-crew2";
const CREW3: &str = "chat-sur101-crew3";
const CHAT_PLAIN: &str = "chat-sur101-plain";
const DEVICE: &str = "dev-captains";
const CAPTAINS_ENV: &str = "ZERON_DEEPSEEK_CAPTAINS";

/// A one-turn hold between an answer's `TextDelta` and its `Done`, so a test
/// can move the roster while the turn is still in flight. `watch`, not
/// `Notify`: a `release` that lands before `pass` awaits must still be
/// observed — a watch receiver reads the current value, not only a wake-up.
struct Gate {
    /// `true` while the next `pass` should wait; `pass` takes the arming.
    armed: AtomicBool,
    /// `false` holds the armed turn, `true` lets it finish. Starts released.
    released: watch::Sender<bool>,
}

impl Gate {
    /// Disarmed and released: `pass` is a no-op until `hold`.
    fn new() -> Arc<Self> {
        let (released, _) = watch::channel(true);
        Arc::new(Self {
            armed: AtomicBool::new(false),
            released,
        })
    }

    /// Arm for the next dispatched turn: that `pass` waits, later ones don't.
    /// `send_replace`, not `send`: `send` discards the value when no receiver
    /// is currently subscribed, which is exactly the case `pass` creates.
    fn hold(&self) {
        self.armed.store(true, Ordering::SeqCst);
        self.released.send_replace(false);
    }

    /// Let a held turn finish. Idempotent.
    fn release(&self) {
        self.released.send_replace(true);
    }

    /// `Agent::run` calls this after the answer `TextDelta`, before `Done`.
    /// Disarmed returns at once — the stream stays byte-identical to an
    /// ungated run; armed consumes the arming and parks until `release`,
    /// including a `release` that already landed.
    async fn pass(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        let mut rx = self.released.subscribe();
        let _ = rx.wait_for(|released| *released).await;
    }
}

/// One persistent agent process per `run()`: answers the launch prompt, then
/// parks warm and answers every steer the mailbox delivers. Reports DeepSeek
/// so `PinRunEnvironment` stamps the run with the chat's Captain role.
struct Agent {
    runs: Arc<Mutex<Vec<RunRequest>>>,
    gate: Arc<Gate>,
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
        let gate = self.gate.clone();
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
            {
                return;
            }
            gate.pass().await;
            if !send(done(DoneStatus::Completed)).await {
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
    gate: Arc<Gate>,
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
            gate: Gate::new(),
        }
    }

    /// One start of the engine on this machine's disk — the test boots once.
    fn boot(&self) -> EngineCore {
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(Agent {
            runs: self.runs.clone(),
            gate: self.gate.clone(),
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

async fn create_chat_with(core: &EngineCore, chat: &str) {
    let client = zeron_rpc::memory_client(core.rpc_service());
    let _ = client
        .call(
            zeron_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": chat, "deviceId": core.device_id }),
        )
        .await;
    let _ = core.workspace.rename_chat(chat, "Pre-titled");
}

async fn create_chat(core: &EngineCore) {
    create_chat_with(core, CHAT).await;
}

fn send_to(core: &EngineCore, chat: &str, id: &str, prompt: &str) {
    core.doc_host
        .queue_command(
            chat,
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

fn send(core: &EngineCore, id: &str, prompt: &str) {
    send_to(core, CHAT, id, prompt);
}

/// The reply for `prompt` folded complete into the doc.
fn answered_in(core: &EngineCore, chat: &str, prompt: &str) -> bool {
    let text = format!("answered: {prompt}");
    core.doc_host
        .open(chat)
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

fn answered(core: &EngineCore, prompt: &str) -> bool {
    answered_in(core, CHAT, prompt)
}

/// The turn is over and the runtime sits warm, mailbox open — not in flight.
fn parked_in(core: &EngineCore, chat: &str) -> bool {
    core.sessions
        .session_status(chat)
        .is_some_and(|s| s.status == SessionStatus::Idle)
}

fn parked(core: &EngineCore) -> bool {
    parked_in(core, CHAT)
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

/// Where a chat's turn-end write lands, mirroring
/// `AgentAccountsConfig::detect` (`deepseek_home`) + `captains::drive_dir` +
/// `needs_you::FILE` exactly: `<deepseek_home>/captains/<chat>/state/needs-you.json`.
fn needs_you_path(data_dir: &std::path::Path, chat: &str) -> std::path::PathBuf {
    let home = std::env::var_os("ZERON_DEEPSEEK_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| data_dir.join("deepseek"));
    home.join("captains").join(chat).join("state").join("needs-you.json")
}

/// `<deepseek home>/captains/<captain>/reports/<chat>.md`, mirroring
/// `captains::drive_dir` + the `reports/` folder.
fn crew_report_path(data_dir: &std::path::Path, captain: &str, chat: &str) -> std::path::PathBuf {
    let home = std::env::var_os("ZERON_DEEPSEEK_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| data_dir.join("deepseek"));
    home.join("captains")
        .join(captain)
        .join("reports")
        .join(format!("{chat}.md"))
}

/// The roster row set for one Captain `cap` with `crew` and no other rows.
fn crew_roster(captain: &str, name: &str, crew: &[&str]) -> Vec<RosterRow> {
    vec![RosterRow {
        chat_id: captain.into(),
        name: name.into(),
        created_at: chrono::Utc::now(),
        archived: false,
        crew: crew.iter().map(|id| id.to_string()).collect(),
    }]
}

/// `needs-you.json` is written off the event loop (`spawn_blocking` in
/// `captain_turn_ended`), so `parked` alone cannot order against it. Poll for
/// the file up to `within` and report what was seen — the caller's assertion,
/// not this poll, carries the verdict.
async fn file_lands(path: &std::path::Path, within: Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    while !path.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    path.exists()
}

/// One `captains.json` row, serialized exactly as `captains::Captain`
/// (`rename_all = "camelCase"`): the shape `Registry::load` parses.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterRow {
    chat_id: String,
    name: String,
    created_at: chrono::DateTime<chrono::Utc>,
    archived: bool,
    crew: Vec<String>,
}

/// Read `dir/captains.json` the way `Registry::load` does: parse it back into
/// the same row shape, so a file the engine's loader would refuse fails the
/// test at the read, not inside the turn-end writer.
fn read_roster(dir: &std::path::Path) -> Vec<RosterRow> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RegistryFile {
        captains: Vec<RosterRow>,
    }
    let raw = std::fs::read_to_string(dir.join("captains.json")).expect("captains.json reads");
    serde_json::from_str::<RegistryFile>(&raw)
        .expect("captains.json parses as a Captains registry")
        .captains
}

/// Replace `dir/captains.json` with `rows` — a mid-turn roster move. The
/// roster RPCs (`CaptainsRpc::row`) refuse any chat with a turn in flight, so
/// the only way a turn can straddle a roster change is for the file itself to
/// change under it: exactly what `captain_turn_ended` loads at `Done`.
fn write_roster(dir: &std::path::Path, captains: Vec<RosterRow>) {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RegistryFile {
        captains: Vec<RosterRow>,
    }
    std::fs::write(
        dir.join("captains.json"),
        serde_json::to_string_pretty(&RegistryFile { captains }).unwrap(),
    )
    .expect("captains.json writes");
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

/// SUR-201: the Needs You file must describe the role the turn was DISPATCHED
/// with. Promoting the chat while its ordinary turn is in flight must not
/// turn that turn into a Captain's — on current code `captain_turn_ended`
/// re-reads the roster at `Done` and writes the file anyway.
#[tokio::test(flavor = "multi_thread")]
async fn a_promote_during_an_ordinary_turn_writes_no_needs_you() {
    let rig = Rig::new();
    let core = rig.boot();
    create_chat_with(&core, CHAT_PROMOTE).await;

    // The gate parks the turn between its answer and its `Done`; the promote
    // lands while the turn is still in flight.
    rig.gate.hold();
    send_to(&core, CHAT_PROMOTE, "m1", "held");
    wait_for(
        || rig.recorded().len() == 1,
        "the held run to be dispatched",
    )
    .await;

    // The roster RPCs refuse a chat with a turn in flight
    // (`CaptainsRpc::row`), so the mid-turn promote lands on `captains.json`
    // itself — the file `captain_turn_ended` loads when the `Done` is
    // published. `read_roster` parses it back as the engine's loader would.
    write_roster(
        &rig.dir,
        vec![RosterRow {
            chat_id: CHAT_PROMOTE.into(),
            name: "Helm".into(),
            created_at: chrono::Utc::now(),
            archived: false,
            crew: Vec::new(),
        }],
    );
    assert!(
        read_roster(&rig.dir)
            .iter()
            .any(|row| row.chat_id == CHAT_PROMOTE && row.name == "Helm" && !row.archived),
        "the mid-turn promote must read back as a live Captain"
    );

    rig.gate.release();
    wait_for(
        || answered_in(&core, CHAT_PROMOTE, "held") && parked_in(&core, CHAT_PROMOTE),
        "the held turn",
    )
    .await;

    // A turn-end write, if there is one, lands off the event loop: give a
    // stray write a short window before asserting absence.
    let path = needs_you_path(&rig.dir, CHAT_PROMOTE);
    file_lands(&path, Duration::from_secs(3)).await;
    assert!(
        !path.exists(),
        "a turn dispatched as an ordinary chat must not write a Needs You file when the \
         roster promotes the chat mid-turn: the file at {path:?} was written from the \
         turn-end roster instead of the role the turn was dispatched with"
    );

    core.shutdown().await;
}

/// The mirror image: a turn dispatched AS a Captain keeps its file even when
/// the demote lands mid-turn. On current code the turn-end roster read finds
/// the archived row and writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_demote_during_a_captain_turn_still_writes_the_file() {
    let rig = Rig::new();
    let core = rig.boot();
    create_chat_with(&core, CHAT_DEMOTE).await;
    let client = zeron_rpc::memory_client(core.rpc_service());

    client
        .call(
            zeron_rpc::methods::PROMOTE_CAPTAIN,
            serde_json::json!({ "chatId": CHAT_DEMOTE, "name": "Helm" }),
        )
        .await
        .expect("PromoteCaptain");
    wait_roster(&client, "the promote readback", |roster| {
        roster["captains"].as_array().is_some_and(|captains| {
            captains.iter().any(|c| {
                c["chatId"] == CHAT_DEMOTE && c["name"] == "Helm" && c["archived"] == false
            })
        })
    })
    .await;

    rig.gate.hold();
    send_to(&core, CHAT_DEMOTE, "m2", "held");
    wait_for(
        || rig.recorded().len() == 1,
        "the held run to be dispatched",
    )
    .await;

    // The same in-flight gate bars the demote RPC too: a real demote keeps
    // the record and crew and flips `archived`, so land that on
    // `captains.json` — the file `captain_turn_ended` loads at `Done`.
    let mut roster = read_roster(&rig.dir);
    roster
        .iter_mut()
        .find(|row| row.chat_id == CHAT_DEMOTE)
        .expect("the promoted row is on disk")
        .archived = true;
    write_roster(&rig.dir, roster);
    assert!(
        read_roster(&rig.dir)
            .iter()
            .any(|row| row.chat_id == CHAT_DEMOTE && row.archived),
        "the mid-turn demote must read back as an archived Captain"
    );

    rig.gate.release();
    wait_for(
        || answered_in(&core, CHAT_DEMOTE, "held") && parked_in(&core, CHAT_DEMOTE),
        "the held turn",
    )
    .await;

    let path = needs_you_path(&rig.dir, CHAT_DEMOTE);
    file_lands(&path, Duration::from_secs(15)).await;
    assert!(
        path.exists(),
        "a turn dispatched as a Captain must still write its Needs You file when the \
         roster demotes the chat mid-turn; nothing was written at {path:?} — the writer \
         keyed off the turn-end roster instead of the dispatch-time role"
    );

    core.shutdown().await;
}

/// Guard: a sidebar archive (`setChatArchived`) is a view concern — it moves
/// the workspace row, not the roster — so a live Captain's turn still writes
/// its file. The archive must land mid-turn: `queue_command` unarchives the
/// row for any message-bearing send (`unarchive_on_send`), so archiving first
/// would silently test an unarchived chat.
#[tokio::test(flavor = "multi_thread")]
async fn a_sidebar_archived_captain_turn_still_writes_the_file() {
    let rig = Rig::new();
    let core = rig.boot();
    create_chat_with(&core, CHAT_SIDEBAR).await;
    let client = zeron_rpc::memory_client(core.rpc_service());

    client
        .call(
            zeron_rpc::methods::PROMOTE_CAPTAIN,
            serde_json::json!({ "chatId": CHAT_SIDEBAR, "name": "Helm" }),
        )
        .await
        .expect("PromoteCaptain");
    wait_roster(&client, "the promote readback", |roster| {
        roster["captains"].as_array().is_some_and(|captains| {
            captains.iter().any(|c| {
                c["chatId"] == CHAT_SIDEBAR && c["name"] == "Helm" && c["archived"] == false
            })
        })
    })
    .await;

    rig.gate.hold();
    send_to(&core, CHAT_SIDEBAR, "m3", "still here");
    wait_for(
        || rig.recorded().len() == 1,
        "the held run to be dispatched",
    )
    .await;

    // While the turn is held, sidebar-archive the chat on the workspace row.
    client.call(zeron_rpc::methods::MUTATE, serde_json::json!({
        "op": "setChatArchived", "chatId": CHAT_SIDEBAR, "archived": true
    })).await.expect("setChatArchived");

    // A sidebar archive is a view concern: the roster row must not move.
    let roster = client
        .call(zeron_rpc::methods::CAPTAINS, serde_json::json!({}))
        .await
        .expect("the Captains roster reads");
    assert!(
        roster["captains"].as_array().is_some_and(|captains| {
            captains
                .iter()
                .any(|c| c["chatId"] == CHAT_SIDEBAR && c["archived"] == false)
        }),
        "a sidebar archive must not change the roster; the Captain row read back archived"
    );

    rig.gate.release();
    wait_for(
        || answered_in(&core, CHAT_SIDEBAR, "still here") && parked_in(&core, CHAT_SIDEBAR),
        "the held turn",
    )
    .await;

    let path = needs_you_path(&rig.dir, CHAT_SIDEBAR);
    file_lands(&path, Duration::from_secs(15)).await;
    assert!(
        path.exists(),
        "a sidebar archive is a view concern: the Captain's turn must still write its \
         Needs You file, and nothing was written at {path:?}"
    );

    core.shutdown().await;
}

/// SUR-101 (red): a crew chat's turn must leave its report in the CAPTAIN's
/// drive — `<drive>/reports/<chatId>.md` — naming the crew chat, the turn's
/// `completed` status, when it ran, and its last assistant text. Nothing
/// writes that file today, so this fails at `file_lands`.
#[tokio::test(flavor = "multi_thread")]
async fn a_crew_turn_writes_its_report_into_the_captains_drive() {
    let rig = Rig::new();
    write_roster(&rig.dir, crew_roster(CAP, "Ada", &[CREW]));
    let core = rig.boot();
    create_chat_with(&core, CREW).await;

    send_to(&core, CREW, "m1", "hold the line");
    wait_for(
        || answered_in(&core, CREW, "hold the line") && parked_in(&core, CREW),
        "the crew turn",
    )
    .await;

    // Like `needs-you.json`, the report is written off the event loop, so
    // `parked` cannot order against it: poll for the file.
    let path = crew_report_path(&rig.dir, CAP, CREW);
    assert!(
        file_lands(&path, Duration::from_secs(15)).await,
        "a crew turn must write its report at {path:?}; nothing was written"
    );
    let body = std::fs::read_to_string(&path).expect("the report reads");
    assert!(
        body.starts_with(&format!("# Crew report: {CREW}\n")),
        "the report names the crew chat: {body:?}"
    );
    assert!(
        body.contains("- Status: completed\n"),
        "a completed crew turn reports completed: {body:?}"
    );
    let at = body
        .lines()
        .find_map(|l| l.strip_prefix("- At: "))
        .expect("the report carries an At line");
    chrono::DateTime::parse_from_rfc3339(at).expect("At is an ISO timestamp");
    assert!(
        body.contains("answered: hold the line"),
        "the report carries the turn's last assistant text: {body:?}"
    );

    core.shutdown().await;
}

/// SUR-101 (red): the report is per chat, not per turn — a second turn on the
/// same crew chat replaces the first report, so the file always reads as the
/// LATEST turn. Nothing writes it today, so this fails at `file_lands`.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_crew_turn_replaces_the_first_report() {
    let rig = Rig::new();
    write_roster(&rig.dir, crew_roster(CAP, "Ada", &[CREW2]));
    let core = rig.boot();
    create_chat_with(&core, CREW2).await;

    send_to(&core, CREW2, "m1", "first");
    wait_for(
        || answered_in(&core, CREW2, "first") && parked_in(&core, CREW2),
        "the first crew turn",
    )
    .await;
    send_to(&core, CREW2, "m2", "second");
    wait_for(
        || answered_in(&core, CREW2, "second") && parked_in(&core, CREW2),
        "the second crew turn",
    )
    .await;

    let path = crew_report_path(&rig.dir, CAP, CREW2);
    assert!(
        file_lands(&path, Duration::from_secs(15)).await,
        "a crew turn must write its report at {path:?}; nothing was written"
    );
    // The second turn's write lands off the event loop too: once the path
    // exists the file can still hold the FIRST report, so wait until it
    // shows the second turn before reading it.
    wait_for(
        || {
            std::fs::read_to_string(&path)
                .is_ok_and(|body| body.contains("answered: second"))
        },
        "the second turn's report",
    )
    .await;
    let body = std::fs::read_to_string(&path).expect("the report reads");
    assert!(
        body.contains("answered: second"),
        "two consecutive turns leave the latest turn in the report: {body:?}"
    );
    assert!(
        !body.contains("answered: first"),
        "the report is replaced, not appended: {body:?}"
    );

    core.shutdown().await;
}

/// SUR-101 (red): a report carries the turn's text but stays bounded — text
/// past 4,000 chars is cut and the file says so with a `[truncated]` note.
/// Nothing writes it today, so this fails at `file_lands`.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_crew_turn_report_is_capped_with_a_note() {
    let rig = Rig::new();
    write_roster(&rig.dir, crew_roster(CAP, "Ada", &[CREW3]));
    let core = rig.boot();
    create_chat_with(&core, CREW3).await;

    let prompt = "x".repeat(5_000);
    send_to(&core, CREW3, "m1", &prompt);
    wait_for(
        || answered_in(&core, CREW3, &prompt) && parked_in(&core, CREW3),
        "the long crew turn",
    )
    .await;

    let path = crew_report_path(&rig.dir, CAP, CREW3);
    assert!(
        file_lands(&path, Duration::from_secs(15)).await,
        "a crew turn must write its report at {path:?}; nothing was written"
    );
    let body = std::fs::read_to_string(&path).expect("the report reads");
    let text = body
        .split_once("\n\n")
        .expect("header then the turn text")
        .1;
    assert!(
        text.starts_with("answered: xxxx"),
        "the body is the turn's text: {:.80}",
        text
    );
    assert!(
        text.chars().count() <= 4_000 + "[truncated]\n".len() + 2,
        "the turn text is capped at 4,000 chars plus the truncation note; saw {}",
        text.chars().count()
    );
    assert!(
        body.contains("[truncated]"),
        "a capped report says it was truncated: {body:?}"
    );

    core.shutdown().await;
}

/// SUR-101 guard — expected green before the write exists: a Captain's OWN
/// turn is not a crew turn. It writes its Needs You state (SUR-201) and must
/// never drop a `reports/<chat>.md` into its own drive.
#[tokio::test(flavor = "multi_thread")]
async fn a_captain_turn_writes_no_crew_report() {
    let rig = Rig::new();
    write_roster(&rig.dir, crew_roster(CAP, "Ada", &[]));
    let core = rig.boot();
    create_chat_with(&core, CAP).await;

    send_to(&core, CAP, "m1", "steady");
    wait_for(
        || answered_in(&core, CAP, "steady") && parked_in(&core, CAP),
        "the captain turn",
    )
    .await;

    // A stray write, were there one, lands off the event loop like the Needs
    // You file: give it a short window before asserting absence.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let path = crew_report_path(&rig.dir, CAP, CAP);
    assert!(
        !path.exists(),
        "a Captain writes its Needs You state, never a crew report; a report \
         was written at {path:?}"
    );

    core.shutdown().await;
}

/// SUR-101 guard — expected green before the write exists: an ordinary chat
/// (no roster row, no Captain) must never produce a `reports/*.md` anywhere
/// under the data dir.
#[tokio::test(flavor = "multi_thread")]
async fn an_ordinary_chat_writes_no_crew_report() {
    let rig = Rig::new();
    let core = rig.boot();
    create_chat_with(&core, CHAT_PLAIN).await;

    send_to(&core, CHAT_PLAIN, "m1", "just a turn");
    wait_for(
        || {
            answered_in(&core, CHAT_PLAIN, "just a turn") && parked_in(&core, CHAT_PLAIN)
        },
        "the ordinary turn",
    )
    .await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut stack = vec![rig.dir.clone()];
    let mut reports = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("the data dir walks") {
            let path = entry.expect("a dir entry reads").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "md")
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|name| name == "reports")
            {
                reports.push(path);
            }
        }
    }
    assert!(
        reports.is_empty(),
        "an ordinary chat writes no crew report; found {reports:?}"
    );

    core.shutdown().await;
}
