//! SUR-230 slice B: the walking skeleton's rig — an in-process `EngineCore`
//! on a temp data dir, a scripted DeepSeek harness answering every turn
//! (and writing the Captain's `DIGEST.md` on the crew-report wake), and the
//! app's own pages (`ui/shell_fixture.luau`, `ui/helm_live_fixture.luau`)
//! mounted with `captains::install_over`'s host actions on the engine's
//! memory transport. `helm_live_tests.rs` drives it: promote through the
//! sidebar's real menu, add the crew, one crew report, the wake it sets
//! off, and the Needs-you strip and Digest tab over the engine's Drive.
//!
//! The harness reports `HarnessId::DeepSeek`: only DeepSeek runs get the
//! `ZERON_DEEPSEEK_CAPTAINS` stamp (`captain_role_runtime.rs` says why), and
//! the stamp is what files the crew report, wakes the Captain and snapshots
//! `state/needs-you.json`.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value as Json, json};
use tokio::sync::mpsc;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};
use zeron_rpc::{RpcReply, RpcService};
use zgx_script::{ScriptHost, testing::TestApp};

/// The fixture's Captain chat (`helm_live_tests`' `CAP`): the wake turn
/// writes this Captain's `DIGEST.md`.
const CAP: &str = "a1";

/// What the scripted Captain's turn leaves in its Drive on a wake: one ask
/// under the Digest's Needs-you section, for the engine's snapshot and the
/// page's strip.
const DIGEST: &str = "# Billing\n\n## Needs you\n\n- Approve the invoice\n\n## New\n\n## In progress\n";

/// A mounted page over the rig's engine: shell or Digest, same wiring —
/// `mcp_live_tests`' `live` does it for the settings page.
pub struct Rig {
    tmp: tempfile::TempDir,
    data_dir: PathBuf,
    deepseek_home: PathBuf,
    core: EngineCore,
    rt: tokio::runtime::Runtime,
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

/// One persistent agent process per `run()`: answers the launch prompt,
/// then parks warm and answers every steer the mailbox delivers —
/// `Agent` in `crates/engine/tests/captain_role_runtime.rs`, plus the
/// `DIGEST.md` write the Captain's Drive gets on a "[crew report]" wake
/// (what a real agent is asked for by the wake's AGENTS.md).
struct Scripted {
    runs: Arc<Mutex<Vec<RunRequest>>>,
    captain: String,
    deepseek_home: PathBuf,
}

#[async_trait]
impl Harness for Scripted {
    fn id(&self) -> HarnessId {
        HarnessId::DeepSeek
    }
    fn display_name(&self) -> &str {
        "Scripted"
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
        let digest = self
            .deepseek_home
            .join("captains")
            .join(&self.captain)
            .join("DIGEST.md");
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
            // The wake: the Captain's turn writes its Digest before it
            // answers, so the engine's turn-end snapshot reads the new list.
            if request.prompt.starts_with("[crew report]") {
                if let Some(dir) = digest.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&digest, DIGEST);
            }
            if !send(AgentEvent::TextDelta {
                text: format!("answered: {}", request.prompt),
            })
            .await
            {
                return;
            }
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

impl Rig {
    pub fn new() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("device-id"), "dev-sur230").unwrap();
        // The roster the engine's CaptainsStore opens at assembly — empty;
        // only the roster RPCs write rows after this.
        std::fs::write(data_dir.join("captains.json"), r#"{"captains":[]}"#).unwrap();
        // Captain Drives live under the DeepSeek home, which the engine
        // defaults to `<data>/deepseek` (`AgentAccountsConfig::detect`) — a
        // Drive is `<deepseek_home>/captains/<id>`.
        let deepseek_home = data_dir.join("deepseek");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let runs: Arc<Mutex<Vec<RunRequest>>> = Arc::default();
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(Scripted {
            runs: runs.clone(),
            captain: CAP.to_owned(),
            deepseek_home: deepseek_home.clone(),
        }));
        // `assemble` boots the workspace host, which spawns
        // (`spawn_blocking`) and needs a Tokio context of its own — as
        // `mcp_live_tests`' `live` enters the runtime it keeps.
        let core = {
            let _enter = rt.enter();
            EngineCore::assemble(&data_dir, Arc::new(registry), HarnessId::DeepSeek, None)
                .expect("engine core assembles")
        };
        core.sessions.set_no_ipc_port();
        Rig {
            tmp,
            data_dir,
            deepseek_home,
            core,
            rt,
            runs,
        }
    }

    /// One RPC on the engine's own service, on the rig's runtime.
    pub fn call(&self, method: &str, params: Json) -> Result<Json, String> {
        self.rt.block_on(async {
            match self.core.rpc_service().handle(method, params).await {
                Ok(RpcReply::Value(value)) => Ok(value),
                Ok(_) => Err(format!("{method} returned a stream")),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    /// A chat as the app's send path makes one: the work folder under the
    /// rig's root, the chat on DeepSeek (the harness `send` reports), then
    /// titled — the wake line names the title.
    pub fn create_chat(&self, chat: &str) {
        let cwd = self.tmp.path().join("work").join(chat);
        std::fs::create_dir_all(&cwd).unwrap();
        self.call(
            zeron_rpc::methods::MUTATE,
            json!({
                "op": "createChat", "chatId": chat, "deviceId": self.core.device_id,
                "cwd": cwd,
                "config": {
                    "harness": "deepseek", "model": "deepseek-flash",
                    "modelOptions": { "webSearch": "off" },
                    "sandbox": "workspace-write", "access": "supervised",
                },
            }),
        )
        .expect("createChat");
        self.call(
            zeron_rpc::methods::MUTATE,
            json!({ "op": "renameChat", "chatId": chat, "title": "Pre-titled" }),
        )
        .expect("renameChat");
    }

    /// The app's own send path: a `Run` command through `QueueCommand`, as
    /// the composer sends it.
    pub fn send(&self, chat: &str, prompt: &str) -> Result<(), String> {
        let cwd = self.tmp.path().join("work").join(chat);
        let request: RunRequest = serde_json::from_value(json!({
            "harness": "deepseek", "prompt": prompt, "cwd": cwd,
            "model": "deepseek-flash", "modelOptions": { "webSearch": "off" },
            "sandbox": "workspace-write", "access": "supervised",
        }))
        .map_err(|e| e.to_string())?;
        let request = serde_json::to_value(&request).map_err(|e| e.to_string())?;
        let command = json!({ "kind": "run", "messageId": prompt, "request": request });
        self.call(
            zeron_rpc::methods::QUEUE_COMMAND,
            json!({ "chatId": chat, "command": command }),
        )
        .map(drop)
    }

    /// The main window's shell fixture, with the Captain host actions over
    /// this rig's engine (the sidebar's Promote menu is the test's way in).
    pub fn shell(&self) -> TestApp {
        self.mount("shell_fixture.luau")
    }

    /// The Digest strip and tab over the same live roster and Drive.
    pub fn digest(&self) -> TestApp {
        self.mount("helm_live_fixture.luau")
    }

    /// A page mount on its own memory client, exactly `mcp_live_tests`'
    /// shape: the host actions `captains::install_over` registers talk to
    /// this rig's engine, not the process-wide one.
    fn mount(&self, page: &str) -> TestApp {
        let client = Rc::new({
            let _enter = self.rt.enter();
            zeron_rpc::memory_client(self.core.rpc_service())
        });
        let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
        let fixture = ui.join(page);
        let _enter = self.rt.enter();
        TestApp::new(move |runtime| {
            // The page's modules name their packages as `ui/.luaurc` does
            // (`@ds`, `@screens`, `@account`); the app's own host registers
            // them, so a fixture mounted on a bare `ScriptHost` must too.
            let host = ScriptHost::new(runtime, fixture)
                .package("ds", ui.join("ds"))
                .package("screens", ui.join("screens"))
                .package("account", ui.join("account"));
            crate::captains::install_over(
                &host,
                move |method: &'static str, params: Json| -> crate::providers::McpCall {
                    let client = Rc::clone(&client);
                    Box::pin(async move {
                        client.call(method, params).await.map_err(|error| error.to_string())
                    })
                },
            );
            host
        })
    }

    /// The roster as `captains.json` holds it (empty when missing or
    /// unparsable), the way the engine's own file reads.
    pub fn captains(&self) -> Vec<Json> {
        let text = std::fs::read_to_string(self.data_dir.join("captains.json")).unwrap_or_default();
        let parsed: Json = serde_json::from_str(&text).unwrap_or(Json::Null);
        parsed["captains"].as_array().cloned().unwrap_or_default()
    }

    /// Every `RunRequest` the scripted harness was dispatched, in order.
    pub fn runs(&self) -> Vec<RunRequest> {
        self.runs.lock().unwrap().clone()
    }

    /// `<deepseek_home>/captains/<captain>/reports` — the Captain Drive's
    /// crew reports folder.
    pub fn reports_dir(&self, captain: &str) -> PathBuf {
        self.deepseek_home.join("captains").join(captain).join("reports")
    }

    /// The Captain Drives' root — the engine's `deepseek_home`
    /// (`<data>/deepseek`), so `data_dir().join("captains")` names a Drive.
    pub fn data_dir(&self) -> &Path {
        &self.deepseek_home
    }

    pub fn close(self) {
        // Shut the engine down while the temp dir still exists: dropping it
        // first removes the files underneath a running engine.
        let Rig { core, rt, tmp, .. } = self;
        rt.block_on(async { core.shutdown().await });
        drop(tmp);
    }
}
