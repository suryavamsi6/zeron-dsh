pub mod gateway;
mod normalize;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use futures::{
    StreamExt,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use zeron_proto::{
    AccessMode, AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode, UserInputQuestion,
};

use crate::{
    Harness, HarnessError, ModelCatalog, ModelContext, RunControls,
    process::{Command, Stdio},
};

pub const SDK_VERSION: &str = "0.2.2-rc.3";
pub const DEFAULT_MODEL: &str = "deepseek-flash";
/// The provider route a run takes when the host names none: DSH's own
/// API-key route, which is the one this build has always run.
pub const DEFAULT_PROVIDER: &str = "deepseek-official";
const SHIM: &str = include_str!("shim.mjs");
const POLICY: &str = include_str!("policy.mjs");
const ENVIRONMENT: &str = include_str!("environment.mjs");
const EFFORTS: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::High,
    ReasoningLevel::Max,
];

pub struct DeepSeekHarness {
    sdk_root: PathBuf,
    home: PathBuf,
    #[cfg(test)]
    test_runtime: Option<PathBuf>,
}

fn data_root() -> PathBuf {
    if let Some(root) = std::env::var_os("ZERON_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(crate::executable::home_or_current_dir)
            .join("Zeron");
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::executable::home_or_current_dir().join(".local/share"))
        .join("zeron")
}

fn configured_path(name: &str, fallback: PathBuf) -> PathBuf {
    let path = std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(fallback);
    std::path::absolute(&path).unwrap_or(path)
}

fn node() -> Option<PathBuf> {
    crate::executable::find_on_paths_with(
        "node",
        vec![],
        &|name| std::env::var_os(name),
        crate::shell_env::user_path(),
        crate::executable::Platform::current(),
    )
}

fn sdk_installed(root: &Path) -> bool {
    ["dsh-sdk-client", "dsh", "dsh-sdk-protocol"]
        .into_iter()
        .all(|package| {
            let dir = root.join("node_modules/@deepseek-ai").join(package);
            let manifest = std::fs::read(dir.join("package.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            manifest.is_some_and(|v| v["version"] == SDK_VERSION)
                && dir
                    .join(if package == "dsh" {
                        "lib/bin.js"
                    } else {
                        "lib/index.js"
                    })
                    .is_file()
        })
}

pub fn static_models() -> Vec<Model> {
    [
        (DEFAULT_MODEL, "DeepSeek V4.1 Flash"),
        ("deepseek-v4-pro", "DeepSeek V4 Pro"),
    ]
    .into_iter()
    .map(|(id, label)| Model {
        id: id.into(),
        label: label.into(),
        description: None,
        provider: None,
        reasoning_levels: EFFORTS.to_vec(),
        options: vec![],
        default: id == DEFAULT_MODEL,
    })
    .collect()
}

impl DeepSeekHarness {
    pub fn new() -> Self {
        Self {
            sdk_root: configured_path(
                "ZERON_DEEPSEEK_SDK_DIR",
                data_root().join("deepseek-sdk").join(SDK_VERSION),
            ),
            home: configured_path("ZERON_DEEPSEEK_HOME", data_root().join("deepseek")),
            #[cfg(test)]
            test_runtime: None,
        }
    }

    pub fn discover() -> Self {
        Self::new()
    }

    /// Where the pinned SDK lives, as the gateway service is started from it.
    pub fn sdk_root(&self) -> &Path {
        &self.sdk_root
    }

    /// The harness home: one store of settings and credentials for this
    /// device, read by every session and by the service that configures them.
    pub fn home(&self) -> &Path {
        &self.home
    }
}

impl Default for DeepSeekHarness {
    fn default() -> Self {
        Self::new()
    }
}

fn launch_environment(request: &RunRequest) -> Result<BTreeMap<OsString, OsString>, HarnessError> {
    let allowed = |key: &str| {
        matches!(
            key,
            "PATH"
                | "HOME"
                | "USERPROFILE"
                | "LOCALAPPDATA"
                | "APPDATA"
                | "SystemRoot"
                | "SYSTEMROOT"
                | "WINDIR"
                | "COMSPEC"
                | "PATHEXT"
                | "TMPDIR"
                | "TMP"
                | "TEMP"
                | "LANG"
                | "TZ"
                | "SHELL"
                | "SSL_CERT_FILE"
                | "SSL_CERT_DIR"
                | "NODE_EXTRA_CA_CERTS"
                | "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "ALL_PROXY"
                | "NO_PROXY"
                | "http_proxy"
                | "https_proxy"
                | "all_proxy"
                | "no_proxy"
                | "DEEPSEEK_API_KEY"
                | "DEEPSEEK_BASE_URL"
        ) || key.starts_with("LC_")
    };
    let mut env: BTreeMap<_, _> = std::env::vars_os()
        .filter(|(key, _)| allowed(&key.to_string_lossy()))
        .collect();
    for (key, value) in &request.env {
        if matches!(
            key.as_str(),
            "DEEPSEEK_API_KEY" | "DEEPSEEK_BASE_URL" | "GH_TOKEN"
        ) {
            env.insert(key.into(), value.into());
        }
    }
    if let Some(count) = request.env.get("GIT_CONFIG_COUNT") {
        let count = count
            .parse::<usize>()
            .ok()
            .filter(|count| *count <= 64)
            .ok_or_else(|| {
                HarnessError::Protocol("Invalid trusted Git configuration count".into())
            })?;
        env.insert("GIT_CONFIG_COUNT".into(), count.to_string().into());
        for index in 0..count {
            for part in ["KEY", "VALUE"] {
                let key = format!("GIT_CONFIG_{part}_{index}");
                let value = request.env.get(&key).ok_or_else(|| {
                    HarnessError::Protocol("Incomplete trusted Git configuration".into())
                })?;
                env.insert(key.into(), value.into());
            }
        }
    }
    Ok(env)
}

fn configuration(request: &RunRequest, home: &Path, session: &str) -> Result<Value, HarnessError> {
    let home = request
        .env
        .get("ZERON_DEEPSEEK_HOME")
        .map(Path::new)
        .unwrap_or(home);
    if !home.is_absolute() || home.as_os_str().is_empty() {
        return Err(HarnessError::Protocol(
            "The host must select an absolute DeepSeek home".into(),
        ));
    }
    // The host decides whether this run reads Zeron's own credential store or
    // a fenced per-run file, and which provider route it runs on. Both are
    // host-stamped (`RunRequest::env` never rides the wire), so a sender can
    // neither widen a run's credentials nor pick a route the host did not.
    let shared_home = request
        .env
        .get("ZERON_DEEPSEEK_SHARED_HOME")
        .is_some_and(|value| value == "1");
    let provider = request
        .env
        .get("ZERON_DEEPSEEK_PROVIDER")
        .map(String::as_str)
        .unwrap_or(DEFAULT_PROVIDER);
    if provider.is_empty()
        || provider.len() > 64
        || !provider
            .bytes()
            .enumerate()
            .all(|(at, b)| b.is_ascii_lowercase() || b.is_ascii_digit() || (b == b'-' && at > 0))
    {
        return Err(HarnessError::Protocol(
            "Invalid DeepSeek provider route".into(),
        ));
    }
    let model = request.model.as_deref().unwrap_or(DEFAULT_MODEL);
    if provider == DEFAULT_PROVIDER {
        if ![DEFAULT_MODEL, "deepseek-v4-pro"].contains(&model) {
            return Err(HarnessError::Protocol(
                "Only official deepseek-flash and deepseek-v4-pro models are supported".into(),
            ));
        }
    } else if model.is_empty()
        || model.len() > 128
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'))
    {
        // Any other route's models are the store's to name; DSH resolves them
        // and reports an unknown one itself.
        return Err(HarnessError::Protocol(
            "Invalid model for this provider route".into(),
        ));
    }
    if request.plan {
        return Err(HarnessError::Protocol(
            "DeepSeek plan mode is not exposed by this integration".into(),
        ));
    }
    if session.is_empty()
        || session.len() > 256
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(HarnessError::Protocol(
            "Invalid DeepSeek session identity".into(),
        ));
    }
    // Every level DSH's `reasoningEffort` names: a model's catalog says which
    // of them it accepts (`thinkingLevelMap`), and the picker offers that
    // intersection, so refusing one here would hide a level the model has.
    let effort = match request.reasoning {
        None => None,
        Some(ReasoningLevel::Minimal) => Some("minimal"),
        Some(ReasoningLevel::Low) => Some("low"),
        Some(ReasoningLevel::Medium) => Some("medium"),
        Some(ReasoningLevel::High) => Some("high"),
        Some(ReasoningLevel::XHigh) => Some("xhigh"),
        Some(ReasoningLevel::Max) => Some("max"),
        _ => {
            return Err(HarnessError::Protocol(
                "DeepSeek supports minimal, low, medium, high, xhigh, and max reasoning".into(),
            ));
        }
    };
    if request.model_options.keys().any(|key| key != "maxTokens") {
        return Err(HarnessError::Protocol(
            "Unsupported DeepSeek model option".into(),
        ));
    }
    let max_tokens = request
        .model_options
        .get("maxTokens")
        .map(|value| {
            value
                .as_u64()
                .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
                .ok_or_else(|| {
                    HarnessError::Protocol("maxTokens must be a positive safe integer".into())
                })
        })
        .transpose()?;
    let cwd = std::path::absolute(&request.cwd)?;
    let mut config = json!({
        "type": "start", "sessionId": session, "resume": request.resume.is_some(),
        "dshHome": home, "cwd": cwd, "model": model, "provider": provider,
        "sharedHome": shared_home, "access": request.access,
        "sandbox": request.sandbox, "policySource": POLICY, "environmentSource": ENVIRONMENT,
        "mcp": request.mcp.iter().chain(request.extra_mcp.iter()).collect::<Vec<_>>()
    });
    if let Some(effort) = effort {
        config["reasoningEffort"] = effort.into();
    }
    if let Some(max_tokens) = max_tokens {
        config["maxTokens"] = max_tokens.into();
    }
    Ok(config)
}

#[async_trait]
impl Harness for DeepSeekHarness {
    fn id(&self) -> HarnessId {
        HarnessId::DeepSeek
    }
    fn display_name(&self) -> &str {
        "DeepSeek Harness"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        EFFORTS
    }
    fn access_modes(&self) -> &[AccessMode] {
        &[
            AccessMode::Supervised,
            AccessMode::AutoAcceptEdits,
            AccessMode::FullAccess,
        ]
    }
    fn installed(&self) -> bool {
        node().is_some() && sdk_installed(&self.sdk_root)
    }
    fn executable_path(&self) -> Option<PathBuf> {
        sdk_installed(&self.sdk_root).then(|| {
            self.sdk_root
                .join("node_modules/@deepseek-ai/dsh/lib/bin.js")
        })
    }
    fn authoritative_prompt_end(&self) -> bool {
        true
    }
    fn same_runtime(&self, live: &RunRequest, next: &RunRequest) -> bool {
        live.model == next.model
            && live.reasoning == next.reasoning
            && crate::same_model_options(&live.model_options, &next.model_options)
            && live.cwd == next.cwd
            && live.access == next.access
            && live.plan == next.plan
            && live.sandbox == next.sandbox
            && live.mcp == next.mcp
            && live.extra_mcp == next.extra_mcp
            && live.env == next.env
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(static_models())
    }
    fn fallback_models(&self) -> Vec<Model> {
        static_models()
    }
    async fn model_catalog(&self, _force: bool) -> Result<ModelCatalog, HarnessError> {
        Ok(ModelCatalog {
            models: static_models(),
            source: "sdk-bundled",
        })
    }
    fn model_context(&self) -> Result<Option<ModelContext>, HarnessError> {
        let Some(node) = node() else { return Ok(None) };
        let mut hash = Sha256::new();
        for path in [&node, &self.sdk_root] {
            hash.update(path.as_os_str().as_encoded_bytes());
            if let Ok(meta) = path.metadata() {
                hash.update(format!("{:?}:{}", meta.modified().ok(), meta.len()));
            }
        }
        hash.update(SDK_VERSION);
        Ok(Some(ModelContext {
            hash: format!("{:x}", hash.finalize()),
            binary_path: node,
            binary_version: Some(SDK_VERSION.into()),
        }))
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let session = request
            .resume
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let config = configuration(&request, &self.home, &session)?;
        let node = node().ok_or_else(|| {
            HarnessError::NotInstalled("Node >=24.9 is required for the DeepSeek SDK".into())
        })?;
        if !sdk_installed(&self.sdk_root) {
            return Err(HarnessError::NotInstalled(format!(
                "Install @deepseek-ai/dsh-sdk-client@{SDK_VERSION} and @deepseek-ai/dsh@{SDK_VERSION} into {} (ZERON_DEEPSEEK_SDK_DIR)",
                self.sdk_root.display()
            )));
        }
        let environment = launch_environment(&request)?;
        // The official route's credential, and only its: any other route
        // resolves its own through DSH (a stored `apiKeyEnv` or an account
        // grant), so requiring this key would refuse every gateway model.
        let provider = request
            .env
            .get("ZERON_DEEPSEEK_PROVIDER")
            .map(String::as_str)
            .unwrap_or(DEFAULT_PROVIDER);
        if provider == DEFAULT_PROVIDER
            && !environment
                .get(std::ffi::OsStr::new("DEEPSEEK_API_KEY"))
                .is_some_and(|v| !v.to_string_lossy().trim().is_empty())
        {
            return Err(HarnessError::NotSignedIn(
                "Set DEEPSEEK_API_KEY in DeepSeek provider settings or the launching environment"
                    .into(),
            ));
        }
        let mut command = Command::new(&node);
        command
            .arg("--input-type=module")
            .arg("-e")
            .arg(SHIM)
            .arg(&self.sdk_root)
            .current_dir(&request.cwd)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(test)]
        if let Some(runtime) = &self.test_runtime {
            command.arg(runtime);
        }
        crate::compose_child_path(&mut command, &node);
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn().map_err(|e| {
            crate::executable::spawn_error(e, Some(Path::new(&request.cwd)), || {
                node.display().to_string()
            })
        })?;
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let _lease = controls.execution_lease.clone();
            if let Err(error) = run_child(child, &request, config, &session, controls, &tx).await {
                let error = crate::redact_secrets(&error.to_string());
                let _ = tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(error),
                        session_id: Some(session),
                    }))
                    .await;
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

async fn write(stdin: &mut crate::process::ChildStdin, value: Value) -> Result<(), HarnessError> {
    let mut bytes =
        serde_json::to_vec(&value).map_err(|e| HarnessError::Protocol(e.to_string()))?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
}

async fn run_child(
    child: crate::process::Child,
    request: &RunRequest,
    config: Value,
    session: &str,
    mut controls: RunControls,
    tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
) -> Result<(), HarnessError> {
    let mut child = crate::process::owned::Child::new(child);
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| HarnessError::Protocol("DeepSeek stdin is missing".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::Protocol("DeepSeek stdout is missing".into()))?;
    let stderr = child.stderr.take();
    let tail = crate::StderrTail::default();
    let tail_writer = tail.clone();
    let stderr_task = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            let mut lines = crate::lossy_lines::lossy_lines(stderr);
            while let Ok(Some(line)) = lines.next_line().await {
                tail_writer.push(&crate::redact_secrets(&line));
            }
        }
    });
    let mut lines = crate::lossy_lines::lossy_lines(stdout);
    let mut answers: FuturesUnordered<BoxFuture<'static, (String, Value)>> =
        FuturesUnordered::new();
    let mut normalizer = normalize::Normalizer::new(session.to_owned());
    let mut active = true;
    let mut pending = std::collections::VecDeque::new();
    let result = async {
        write(&mut stdin, config).await?;
        write(&mut stdin, json!({ "type": "prompt", "prompt": request.prompt, "attachments": request.attachments })).await?;
        loop {
            tokio::select! {
                _ = controls.interrupt.cancelled() => {
                    let _ = write(&mut stdin, json!({"type":"stop"})).await;
                    let _ = tokio::time::timeout(Duration::from_secs(16), child.wait()).await;
                    if active { let _ = tx.send(Ok(AgentEvent::Done { status: DoneStatus::Interrupted, result: None, error: None, session_id: Some(session.into()) })).await; }
                    break;
                }
                _ = tx.closed() => {
                    let _ = write(&mut stdin, json!({"type":"stop"})).await;
                    let _ = tokio::time::timeout(Duration::from_secs(16), child.wait()).await;
                    break;
                }
                Some((id, result)) = answers.next(), if !answers.is_empty() => {
                    write(&mut stdin, json!({"type":"answer", "id":id, "result":result})).await?;
                }
                Some(steer) = controls.steering.recv() => {
                    if steer.config.as_ref().is_some_and(|next| next.model != request.model || next.access != request.access || next.reasoning != request.reasoning || next.cwd != request.cwd || next.model_options != request.model_options || next.sandbox != request.sandbox) {
                        return Err(HarnessError::Protocol("DeepSeek runtime configuration changes require a restart".into()));
                    }
                    pending.push_back(steer);
                }
                line = lines.next_line() => {
                    let Some(line) = line? else {
                        return Err(HarnessError::Protocol(crate::crash_message("DeepSeek SDK", child.try_wait().ok().flatten(), &tail)));
                    };
                    let frame: Value = serde_json::from_str(&line).map_err(|_| HarnessError::Protocol("Malformed DeepSeek sidecar frame".into()))?;
                    match frame["type"].as_str() {
                        Some("ready") => {
                            tx.send(Ok(AgentEvent::SessionStarted {
                                harness: HarnessId::DeepSeek, model: request.model.clone().unwrap_or_else(|| DEFAULT_MODEL.into()), tools: vec![],
                                cwd: request.cwd.clone(), session_id: session.into(), assistant_message_id: uuid::Uuid::new_v4().to_string(),
                            })).await.map_err(|_| HarnessError::Protocol("DeepSeek event receiver closed".into()))?;
                        }
                        Some("notification") => {
                            for event in normalizer.notification(&frame)? { let _ = tx.send(Ok(event)).await; }
                        }
                        Some("request") => {
                            let id = frame["id"].as_str().ok_or_else(|| HarnessError::Protocol("Missing host request identity".into()))?.to_owned();
                            let params = &frame["params"];
                            let method = frame["method"].as_str().unwrap_or("");
                            if method == "approval/request" {
                                let tool = params["toolName"].as_str().unwrap_or("unknown tool");
                                if request.sandbox == SandboxLevel::ReadOnly {
                                    write(&mut stdin, json!({"type":"answer","id":id,"result":{"outcome":"rejected"}})).await?;
                                } else if request.access == AccessMode::FullAccess {
                                    write(&mut stdin, json!({"type":"answer","id":id,"result":{"outcome":"allowed-once"}})).await?;
                                } else {
                                    let question = crate::approval::tool("DeepSeek Harness", tool, params["reason"].as_str());
                                    let response = (controls.request_input)(vec![question.clone()]);
                                    answers.push(Box::pin(async move {
                                        let response = response.await.unwrap_or_default();
                                        (id, json!({"outcome": if crate::approval::approved(&question, &response) {"allowed-once"} else {"rejected"}}))
                                    }));
                                }
                            } else if method == "user-question/ask" {
                                let questions = host_questions(params)?;
                                let response = (controls.request_input)(questions.clone());
                                answers.push(Box::pin(async move {
                                    let response = response.await.unwrap_or_default();
                                    if response.len() != questions.len() { return (id, json!({"cancelled":true})) }
                                    let mut encoded = Vec::new();
                                    for question in questions {
                                        let Some(answer) = response.iter().find(|a| a.question_id == question.id) else { return (id, json!({"cancelled":true})) };
                                        let selected: Vec<_> = answer.labels.iter().filter(|s| question.options.contains(s)).collect();
                                        let custom = answer.labels.iter().filter(|s| !question.options.contains(s)).cloned().collect::<Vec<_>>().join("\n");
                                        let mut answer = json!({"id":question.id,"selected":selected});
                                        if !custom.is_empty() { answer["custom"] = custom.into(); }
                                        encoded.push(answer);
                                    }
                                    (id, json!({"answers":encoded}))
                                }));
                            } else {
                                write(&mut stdin, json!({"type":"answer","id":id,"result":{"cancelled":true}})).await?;
                            }
                        }
                        Some("requestCancelled") => {}
                        Some("done") => {
                            let reason = frame["finishReason"].as_str().unwrap_or("error");
                            let status = match reason { "completed" | "max-tokens" => DoneStatus::Completed, "aborted" => DoneStatus::Interrupted, _ => DoneStatus::Errored };
                            let error = (status == DoneStatus::Errored).then(|| normalizer.error.clone().unwrap_or_else(|| format!("DeepSeek activity ended: {reason}")));
                            let _ = tx.send(Ok(AgentEvent::Done { status, result: frame["finalResponse"].as_str().map(str::to_owned), error, session_id: Some(session.into()) })).await;
                            active = false;
                        }
                        Some("error") => return Err(HarnessError::Protocol(frame["message"].as_str().unwrap_or("DeepSeek SDK failed").into())),
                        Some("stopped") => break,
                        _ => return Err(HarnessError::Protocol("Unknown DeepSeek sidecar frame".into())),
                    }
                }
            }
            if !active && let Some(steer) = pending.pop_front() {
                let _ = tx.send(Ok(AgentEvent::Steered { assistant_message_id: steer.message_id, next_assistant_message_id: Some(uuid::Uuid::new_v4().to_string()) })).await;
                write(&mut stdin, json!({"type":"prompt","prompt":steer.prompt,"attachments":steer.attachments})).await?;
                active = true;
                normalizer.error = None;
            }
        }
        Ok(())
    }.await;
    drop(stdin);
    child.shutdown(Duration::from_secs(2)).await;
    stderr_task.abort();
    let _ = stderr_task.await;
    result
}

fn host_questions(params: &Value) -> Result<Vec<UserInputQuestion>, HarnessError> {
    let source = params["questions"]
        .as_array()
        .filter(|q| !q.is_empty())
        .ok_or_else(|| HarnessError::Protocol("Invalid SDK questions".into()))?;
    source
        .iter()
        .map(|q| {
            Ok(UserInputQuestion {
                id: q["id"]
                    .as_str()
                    .ok_or_else(|| HarnessError::Protocol("Missing question id".into()))?
                    .into(),
                header: q["header"].as_str().unwrap_or("DeepSeek Harness").into(),
                question: format!(
                    "{}{}",
                    q["question"]
                        .as_str()
                        .ok_or_else(|| HarnessError::Protocol("Missing question text".into()))?,
                    q["detail"]
                        .as_str()
                        .map(|s| format!("\n\n{s}"))
                        .unwrap_or_default()
                ),
                options: q["options"]
                    .as_array()
                    .map(|options| {
                        options
                            .iter()
                            .filter_map(|v| v["label"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                multi_select: q["multiSelect"].as_bool().unwrap_or(false),
                prefill: None,
                multiline: false,
            })
        })
        .collect()
}
