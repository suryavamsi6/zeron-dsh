//! Devin harness: spawns Cognition's Devin CLI as an Agent Client Protocol
//! server (`devin acp`, JSON-RPC 2.0 over stdio, protocol v1) and maps its
//! session updates onto [`AgentEvent`]s.
//!
//! Scope of this driver, honestly:
//!
//! - `initialize` (protocolVersion 1, fs/terminal capabilities declined —
//!   Devin's own sandbox owns the working tree) → `session/new`, or
//!   `session/load` with a fresh-session fallback when resuming.
//! - `session/prompt` owns the turn: its response's `stopReason` ends it
//!   (`cancelled` → Interrupted, `refusal`/`error` → Errored, else
//!   Completed). `session/update` notifications normalize per
//!   [`normalize::map_update`].
//! - Permission requests auto-accept the agent's preferred allow option:
//!   Devin is full-access-only here (its `bypass` mode is the one mode value
//!   known), matching unattended parity.
//! - Steering is step-boundary preemption (Devin advertises no
//!   `_session/steering` extension): a mid-turn steer cancels the in-flight
//!   prompt and continues the turn with the steer once it settles.
//! - Auth is the CLI's own (`devin auth login`): an `auth_required` failure
//!   ends the run with instructions rather than opening a flow nobody is
//!   watching.
//!
//! Follow-ups not yet here: live model discovery/selection (runs use the
//! CLI's configured default), slash-command discovery, plan mode, usage
//! accounting.

pub mod normalize;

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use zeron_proto::{
    AccessMode, AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode,
};

use crate::jsonrpc::{Incoming, RpcClient};
use crate::process::{Command, Stdio};
use crate::process::owned as child;
use crate::{Harness, HarnessError, RunControls};
use child::Child;
use normalize::{map_update, preferred_allow_option};

/// Grace between `session/cancel` and SIGTERM on interrupt, then SIGKILL.
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const KILL_GRACE: Duration = Duration::from_secs(3);
/// Bound on the initialize → session handshake; a hang past it is a wedged
/// agent (commonly: waiting on a login), not a slow one.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(120);
/// `session/load`'s own budget: the history replay is disk-local work.
const SESSION_LOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the official installer puts the launcher (`~/.local/bin/devin`
/// symlinks into `~/.local/share/devin/cli/_versions`), plus the system
/// install dirs.
fn install_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = crate::executable::home_or_current_dir();
    dirs.push(home.join(".local").join("bin").join("devin"));
    dirs.push(PathBuf::from("/opt/homebrew/bin/devin"));
    dirs.push(PathBuf::from("/usr/local/bin/devin"));
    dirs
}

/// The `devin` binary: `DEVIN_EXECUTABLE` first, then PATH, then the known
/// install locations.
fn resolve_executable() -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("DEVIN_EXECUTABLE") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Some(path);
        }
    }
    crate::executable::find_on_paths_with(
        "devin",
        install_candidates(),
        &|name| std::env::var_os(name),
        crate::shell_env::user_path(),
        crate::executable::Platform::current(),
    )
}

pub struct DevinHarness {
    /// Test-only executable override, same seam as the DeepSeek harness's
    /// `test_runtime`: integration shims cannot ride `DEVIN_EXECUTABLE`
    /// (process-global env is racy across parallel tests).
    #[cfg(test)]
    pub test_executable: Option<PathBuf>,
}

impl DevinHarness {
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            test_executable: None,
        }
    }
}

impl Default for DevinHarness {
    fn default() -> Self {
        Self::new()
    }
}

fn fallback_models() -> Vec<Model> {
    vec![
        Model {
            default: true,
            id: "adaptive".into(),
            label: "Adaptive".into(),
            provider: None,
            description: Some("Devin picks the model per request".into()),
            reasoning_levels: Vec::new(),
            options: Vec::new(),
        },
        Model {
            default: false,
            id: "swe-1-7-medium".into(),
            label: "SWE-1.7 Medium".into(),
            provider: None,
            description: Some("Devin's own coding model".into()),
            reasoning_levels: Vec::new(),
            options: Vec::new(),
        },
    ]
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientInfo": {
            "name": "zeron",
            "title": "Zeron",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            // Devin otherwise exposes only the parent's run_subagent call:
            // this unlocks lifecycle tags plus nested message, thought and
            // tool updates.
            "_meta": { "cognition.ai/subagentSupport": true },
        },
    })
}

/// The ACP `mcpServers` session field, zeron's own bridge first.
fn session_mcp_servers(request: &RunRequest) -> Vec<Value> {
    request
        .mcp
        .iter()
        .chain(request.extra_mcp.iter())
        .map(|mcp| {
            json!({
                "name": mcp.name,
                "command": mcp.command,
                "args": mcp.args,
                "env": mcp
                    .env
                    .iter()
                    .map(|(name, value)| json!({ "name": name, "value": value }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn is_auth_required(error: &HarnessError) -> bool {
    let message = error.to_string();
    message.contains("auth_required") || message.contains("Authentication required")
}

const SIGN_IN_HINT: &str = "sign in with `devin auth login` in a terminal first";

/// One prompt's content blocks: the text. Staged image attachments ride
/// the prompt text as `Attached images (local files …)` refs (zeron's
/// `withAttachments` transport); Devin reads them from disk itself.
fn prompt_content(text: &str, _attachments: &[String], _inline_images: bool) -> Vec<Value> {
    vec![json!({ "type": "text", "text": text })]
}

#[async_trait]
impl Harness for DevinHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Devin
    }

    fn display_name(&self) -> &str {
        "Devin"
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }

    fn installed(&self) -> bool {
        resolve_executable().is_some()
    }

    fn executable_path(&self) -> Option<PathBuf> {
        resolve_executable()
    }

    fn access_modes(&self) -> &[AccessMode] {
        &[AccessMode::FullAccess]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        // Selection rides a follow-up: the CLI runs its configured default
        // until then, and the picker shows these so the choice is honest
        // about being a no-op.
        Ok(fallback_models())
    }

    fn fallback_models(&self) -> Vec<Model> {
        fallback_models()
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        #[cfg(test)]
        let resolved = self.test_executable.clone().or_else(resolve_executable);
        #[cfg(not(test))]
        let resolved = resolve_executable();
        let executable = resolved.ok_or_else(|| {
            HarnessError::NotInstalled(
                "devin (searched PATH, ~/.local/bin, /opt/homebrew/bin, /usr/local/bin; \
                 install with `curl -fsSL https://cli.devin.ai/install.sh | bash`; \
                 set DEVIN_EXECUTABLE to override)"
                    .into(),
            )
        })?;
        let (event_tx, event_rx) = mpsc::channel(256);
        tokio::spawn(async move {
            drive(executable, request, controls, event_tx).await;
        });
        Ok(futures::stream::unfold(event_rx, |mut rx| async {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

/// Answer a permission request with the agent's preferred allow option
/// (full-access-only agent: unattended parity). Anything else the server
/// asks gets a protocol-level refusal.
fn answer_server_request(client: &RpcClient, id: Value, method: &str, params: &Value) {
    match method {
        "session/request_permission" => {
            let options: Vec<Value> = params
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // A request whose options lack allow/reject kinds is a QUESTION
            // the agent relays this way, not a tool permission; with no
            // input bridge wired here yet it is declined rather than
            // answered blindly.
            let is_question = options.iter().any(|option| {
                !matches!(
                    option.get("kind").and_then(Value::as_str),
                    Some("allow_once" | "allow_always" | "reject_once" | "reject_always")
                )
            });
            let preferred = if is_question {
                None
            } else {
                preferred_allow_option(&options)
            };
            match preferred {
                Some(option_id) => client.respond(
                    &id,
                    json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                ),
                None => client.respond(&id, json!({ "outcome": { "outcome": "cancelled" } })),
            }
        }
        _ => {
            client.respond_error(&id, -32601, &format!("unsupported method: {method}"));
        }
    }
}

struct ChildIo {
    child: Child,
    client: RpcClient,
    incoming: mpsc::Receiver<Incoming>,
}

async fn spawn_devin(executable: &PathBuf, request: &RunRequest) -> Result<ChildIo, HarnessError> {
    let mut command = Command::new(executable);
    command.arg("acp");
    command.current_dir(&request.cwd);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::null());
    command.kill_on_drop(true);
    for (name, value) in &request.env {
        command.env(name, value);
    }
    crate::compose_child_path(&mut command, executable);
    #[cfg(unix)]
    command.process_group(0);
    let spawned = command.spawn().map_err(|err| {
        crate::executable::spawn_error(err, Some(std::path::Path::new(&request.cwd)), || {
            executable.display().to_string()
        })
    })?;
    let mut child = child::Child::new(spawned);
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(HarnessError::Protocol("devin acp child has no stdio".into()));
    };
    let (client, incoming) = RpcClient::new(stdin, stdout);
    Ok(ChildIo {
        child,
        client,
        incoming,
    })
}

/// `session/new`, or `session/load` with the fresh-session fallback when a
/// resume id is unrestorable (missing/foreign). A slow or interrupted
/// restore is retried by the next message rather than replaced.
async fn open_session(
    client: &RpcClient,
    request: &RunRequest,
) -> Result<String, HarnessError> {
    let params = json!({
        "cwd": request.cwd,
        "mcpServers": session_mcp_servers(request),
    });
    if let Some(resume) = &request.resume {
        let mut restore = params.clone();
        restore["sessionId"] = Value::String(resume.clone());
        let restored = tokio::time::timeout(
            SESSION_LOAD_TIMEOUT,
            client.request("session/load", restore),
        )
        .await
        .unwrap_or_else(|_| {
            Err(HarnessError::Protocol(
                "session/load: the history replay did not finish in time".into(),
            ))
        });
        match restored {
            Ok(_) => return Ok(resume.clone()),
            Err(err) if is_auth_required(&err) => return Err(err),
            Err(err) => {
                tracing::debug!("session/load failed, starting fresh: {err}");
            }
        }
    }
    let new = client.request("session/new", params).await?;
    let session_id = new
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if session_id.is_empty() {
        return Err(HarnessError::Protocol(
            "session/new returned no sessionId".into(),
        ));
    }
    Ok(session_id)
}

fn stop_reason_status(response: &Value) -> DoneStatus {
    match response.get("stopReason").and_then(Value::as_str) {
        Some("cancelled") => DoneStatus::Interrupted,
        Some("refusal") | Some("error") => DoneStatus::Errored,
        _ => DoneStatus::Completed,
    }
}

async fn send_event(
    tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    event: AgentEvent,
) -> bool {
    tx.send(Ok(event)).await.is_ok()
}

/// The runtime: one spawned `devin acp` driven through its turns. The event
/// stream ends with exactly one `Done`.
async fn drive(
    executable: PathBuf,
    request: RunRequest,
    controls: RunControls,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
) {
    let RunControls {
        execution_lease: _lease,
        request_input: _,
        mut steering,
        interrupt,
        turn: _turn,
    } = controls;
    macro_rules! finish {
        ($io:expr, $status:expr, $error:expr, $session:expr) => {{
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: $status,
                    result: None,
                    error: $error,
                    session_id: $session,
                }))
                .await;
            $io.client.close_stdin_for_teardown();
            $io.child.shutdown(KILL_GRACE).await;
            return;
        }};
    }

    let mut io = match spawn_devin(&executable, &request).await {
        Ok(io) => io,
        Err(err) => {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(err.to_string()),
                    session_id: request.resume.clone(),
                }))
                .await;
            return;
        }
    };

    // ---- handshake --------------------------------------------------------
    let setup = async {
        let init = io.client.request("initialize", initialize_params()).await?;
        let inline_images =
            init["agentCapabilities"]["promptCapabilities"]["image"].as_bool() == Some(true);
        let session_id = open_session(&io.client, &request).await?;
        Ok::<(String, bool), HarnessError>((session_id, inline_images))
    };
    let res = tokio::select! {
        res = setup => res,
        _ = tokio::time::sleep(HANDSHAKE_TIMEOUT) => Err(HarnessError::Protocol(format!(
            "Devin did not complete the ACP handshake within {}s (it may be waiting for a login — {SIGN_IN_HINT})",
            HANDSHAKE_TIMEOUT.as_secs()
        ))),
        _ = interrupt.cancelled() => {
            finish!(io, DoneStatus::Interrupted, None, None);
        }
    };
    let (session_id, inline_images) = match res {
        Ok(v) => v,
        Err(err) => {
            let error = if is_auth_required(&err) {
                format!("Devin is not signed in: {SIGN_IN_HINT}")
            } else {
                err.to_string()
            };
            finish!(io, DoneStatus::Errored, Some(error), request.resume.clone());
        }
    };

    if !send_event(&event_tx, AgentEvent::SessionStarted {
        harness: HarnessId::Devin,
        model: request.model.clone().unwrap_or_default(),
        tools: Vec::new(),
        cwd: request.cwd.clone(),
        session_id: session_id.clone(),
        assistant_message_id: new_message_id(),
    }).await {
        finish!(io, DoneStatus::Interrupted, None, Some(session_id.clone()));
    }

    // ---- the turns --------------------------------------------------------
    // One prompt in flight at a time. A mid-turn steer preempts: cancel,
    // then the steer becomes the next prompt once the cancel settles. Steers
    // arriving parked (between turns) prompt directly.
    let mut queued_steer: Option<String> = None;
    let mut prompt_fut: Option<futures::future::BoxFuture<'static, Result<Value, HarnessError>>> = {
        let content = prompt_content(&request.prompt, &request.attachments, inline_images);
        Some(io.client.request_now(
            "session/prompt",
            json!({ "sessionId": session_id, "prompt": content }),
        ))
    };
    let mut preempting = false;

    loop {
        tokio::select! {
            inc = io.incoming.recv() => {
                match inc {
                    Some(Incoming::Notification { method, params }) => {
                        if method == "session/update"
                            && params.get("sessionId").and_then(Value::as_str) == Some(session_id.as_str())
                        {
                            if let Some(update) = params.get("update") {
                                for event in map_update(update) {
                                    if !send_event(&event_tx, event).await {
                                        finish!(io, DoneStatus::Interrupted, None, Some(session_id.clone()));
                                    }
                                }
                            }
                        }
                    }
                    Some(Incoming::Request { id, method, params }) => {
                        answer_server_request(&io.client, id, &method, &params);
                    }
                    Some(Incoming::Eof) | None => {
                        if prompt_fut.is_some() {
                            finish!(io, DoneStatus::Errored, Some(
                                "Devin exited mid-turn".to_owned()
                            ), Some(session_id.clone()));
                        }
                        finish!(io, DoneStatus::Errored, Some(
                            "Devin exited while parked between turns".to_owned()
                        ), Some(session_id.clone()));
                    }
                }
            }
            steer = steering.recv() => {
                match steer {
                    Some(message) => {
                        if prompt_fut.is_some() && !preempting {
                            // Preempt: cancel the generation; the steer goes
                            // out once the cancel settles.
                            preempting = true;
                            queued_steer = Some(message.prompt);
                            io.client.notify("session/cancel", Some(json!({ "sessionId": session_id })));
                        } else if prompt_fut.is_some() {
                            // Already preempting: fold into the queued steer.
                            match &mut queued_steer {
                                Some(queued) => {
                                    queued.push_str("\n\n");
                                    queued.push_str(&message.prompt);
                                }
                                None => queued_steer = Some(message.prompt),
                            }
                        } else {
                            let content = prompt_content(&message.prompt, &message.attachments, inline_images);
                            prompt_fut = Some(io.client.request_now(
                                "session/prompt",
                                json!({ "sessionId": session_id, "prompt": content }),
                            ));
                        }
                    }
                    None => {
                        // Steering channel closed: the engine went away. A
                        // parked runtime just waits; interrupt owns teardown.
                        if prompt_fut.is_none() {
                            finish!(io, DoneStatus::Interrupted, None, Some(session_id.clone()));
                        }
                    }
                }
            }
            res = async { prompt_fut.as_mut().unwrap().await }, if prompt_fut.is_some() => {
                prompt_fut = None;
                match res {
                    Ok(response) => {
                        let status = stop_reason_status(&response);
                        if status != DoneStatus::Completed || preempting {
                            // A preemption's cancelled turn is not the end of
                            // the run: the steer continues it.
                            if let Some(steer) = queued_steer.take() {
                                preempting = false;
                                let content = prompt_content(&steer, &[], inline_images);
                                prompt_fut = Some(io.client.request_now(
                                    "session/prompt",
                                    json!({ "sessionId": session_id, "prompt": content }),
                                ));
                                continue;
                            }
                        }
                        preempting = false;
                        // Done is per TURN: the runtime parks between turns
                        // and the same stream carries the next one. Only the
                        // transport dying or the interrupt ends the stream.
                        let error = (status == DoneStatus::Errored).then(|| {
                            format!(
                                "Devin ended the turn: {}",
                                response
                                    .get("stopReason")
                                    .and_then(Value::as_str)
                                    .unwrap_or("error")
                            )
                        });
                        if !send_event(&event_tx, AgentEvent::Done {
                            status,
                            result: None,
                            error,
                            session_id: Some(session_id.clone()),
                        }).await {
                            finish!(io, DoneStatus::Interrupted, None, Some(session_id.clone()));
                        }
                    }
                    Err(err) => {
                        let error = if is_auth_required(&err) {
                            format!("Devin is not signed in: {SIGN_IN_HINT}")
                        } else {
                            err.to_string()
                        };
                        finish!(io, DoneStatus::Errored, Some(error), Some(session_id.clone()));
                    }
                }
            }
            _ = interrupt.cancelled() => {
                io.client.notify("session/cancel", Some(json!({ "sessionId": session_id })));
                tokio::time::sleep(INTERRUPT_GRACE).await;
                finish!(io, DoneStatus::Interrupted, None, Some(session_id.clone()));
            }
        }
    }
}

fn new_message_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("devin-{nanos}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests;
