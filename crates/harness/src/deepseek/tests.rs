use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

fn request(cwd: &Path) -> RunRequest {
    serde_json::from_value(json!({
        "prompt":"first", "model":null, "reasoning":null, "cwd":cwd,
        "sandbox":"workspace-write", "resume":null, "access":"supervised"
    }))
    .unwrap()
}

#[test]
fn a_run_reads_the_shared_store_only_when_the_host_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    // A run the host did not stamp keeps the fenced per-run file.
    let fenced = configuration(&request, dir.path(), "session").unwrap();
    assert_eq!(fenced["sharedHome"], false);
    assert_eq!(fenced["provider"], DEFAULT_PROVIDER);
    request
        .env
        .insert("ZERON_DEEPSEEK_SHARED_HOME".into(), "1".into());
    let shared = configuration(&request, dir.path(), "session").unwrap();
    assert_eq!(shared["sharedHome"], true);
    // Only the host's own stamp counts: any other value leaves it fenced.
    request
        .env
        .insert("ZERON_DEEPSEEK_SHARED_HOME".into(), "true".into());
    assert_eq!(
        configuration(&request, dir.path(), "session").unwrap()["sharedHome"],
        false
    );
}

#[test]
fn a_provider_route_other_than_the_official_one_names_its_own_models() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    // The official route keeps its two models and refuses anything else.
    request.model = Some("deepseek-chat".into());
    assert!(configuration(&request, dir.path(), "session").is_err());
    // A configured route's models are the store's to name.
    request
        .env
        .insert("ZERON_DEEPSEEK_PROVIDER".into(), "opencode-go".into());
    request.model = Some("deepseek-v4.1-flash".into());
    let config = configuration(&request, dir.path(), "session").unwrap();
    assert_eq!(config["provider"], "opencode-go");
    assert_eq!(config["model"], "deepseek-v4.1-flash");
    // A route is a route, not a path or a sentence.
    for bad in ["", "OpenCode Go", "../escape", "-leading", "a/b"] {
        request
            .env
            .insert("ZERON_DEEPSEEK_PROVIDER".into(), bad.into());
        assert!(
            configuration(&request, dir.path(), "session").is_err(),
            "{bad:?} was accepted as a provider route"
        );
    }
    request
        .env
        .insert("ZERON_DEEPSEEK_PROVIDER".into(), "opencode-go".into());
    for bad in ["", "has space", "line\nbreak"] {
        request.model = Some(bad.into());
        assert!(
            configuration(&request, dir.path(), "session").is_err(),
            "{bad:?} was accepted as a model"
        );
    }
}

#[test]
fn only_verified_official_models_and_efforts_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    assert_eq!(
        static_models()
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["deepseek-flash", "deepseek-v4-pro"]
    );
    assert_eq!(
        configuration(&request, dir.path(), "session").unwrap()["model"],
        DEFAULT_MODEL
    );
    request.model = Some("deepseek-chat".into());
    assert!(configuration(&request, dir.path(), "session").is_err());
    request.model = None;
    // Every level DSH's `reasoningEffort` names goes through; a model's own
    // catalog is what narrows it (the picker offers that intersection).
    for (level, wire) in [
        (ReasoningLevel::Minimal, "minimal"),
        (ReasoningLevel::Low, "low"),
        (ReasoningLevel::Medium, "medium"),
        (ReasoningLevel::High, "high"),
        (ReasoningLevel::XHigh, "xhigh"),
    ] {
        request.reasoning = Some(level);
        assert_eq!(
            configuration(&request, dir.path(), "session").unwrap()["reasoningEffort"],
            wire
        );
    }
    request.reasoning = Some(ReasoningLevel::Ultra);
    assert!(configuration(&request, dir.path(), "session").is_err());
    request.reasoning = Some(ReasoningLevel::Max);
    assert_eq!(
        configuration(&request, dir.path(), "session").unwrap()["reasoningEffort"],
        "max"
    );
    assert!(crate::supports_titles(HarnessId::DeepSeek));
}

#[test]
fn plan_mode_is_passed_to_the_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    assert_eq!(
        configuration(&request, dir.path(), "session").unwrap()["plan"],
        false
    );
    request.plan = true;
    assert_eq!(
        configuration(&request, dir.path(), "session").unwrap()["plan"],
        true
    );
}

#[test]
fn a_resumed_run_keeps_the_plan_toggle_so_the_sidecar_can_leave_plan_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    request.resume = Some("saved".into());
    // The shim sends `/plan off` when a start is a resume and the toggle is
    // off: both keys have to survive into the start config for that to work.
    let config = configuration(&request, dir.path(), "saved").unwrap();
    assert_eq!(config["resume"], true);
    assert_eq!(config["plan"], false);
    request.plan = true;
    let config = configuration(&request, dir.path(), "saved").unwrap();
    assert_eq!(config["resume"], true);
    assert_eq!(config["plan"], true);
}

#[test]
fn plan_mode_is_advertised_now_that_the_sidecar_enters_it() {
    let dir = tempfile::tempdir().unwrap();
    let harness = DeepSeekHarness {
        sdk_root: dir.path().to_path_buf(),
        home: dir.path().join("dsh"),
        test_runtime: None,
    };
    assert!(harness.supports_plan());
}

#[test]
fn a_title_run_takes_a_tool_less_minimal_config_and_no_plan_flag() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    request
        .env
        .insert("DEEPSEEK_API_KEY".into(), "synthetic-deepseek".into());
    // A title run is asked for while the chat's plan toggle is on: the
    // minimal profile has no commands service, so the flag must not ride.
    request.plan = true;
    let config = title_configuration(&request, dir.path(), "session").unwrap();
    assert_eq!(config["title"], true);
    assert_eq!(config["systemPrompt"], crate::TITLE_INSTRUCTIONS);
    assert_eq!(config["maxTokens"], 32);
    assert_eq!(config["mcp"], json!([]));
    assert_eq!(config["resume"], false);
    assert!(config.get("reasoningEffort").is_none());
    assert!(config.get("plan").is_none());
}

#[test]
fn the_web_search_switch_defaults_on_and_turns_search_off_per_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    // Every advertised model offers the switch, on by default.
    for model in static_models() {
        let option = model
            .options
            .iter()
            .find(|option| option.id == "webSearch")
            .expect("webSearch option");
        assert_eq!(option.label, "Web search");
        assert_eq!(
            option
                .choices
                .iter()
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            vec!["on", "off"]
        );
        assert_eq!(option.default_choice, "on");
    }
    // An unpicked switch keeps `web_search` on; "off" is the only spell that
    // turns its search half off for the run.
    assert_eq!(
        configuration(&request, dir.path(), "session").unwrap()["webSearch"],
        true
    );
    for (choice, expected) in [("on", true), ("off", false)] {
        request
            .model_options
            .insert("webSearch".into(), json!(choice));
        assert_eq!(
            configuration(&request, dir.path(), "session").unwrap()["webSearch"],
            expected
        );
    }
    for bad in [json!("maybe"), json!(false), json!(1)] {
        request.model_options.insert("webSearch".into(), bad);
        assert!(matches!(
            configuration(&request, dir.path(), "session"),
            Err(HarnessError::Protocol(_))
        ));
    }
    // The switch rides alongside the other accepted model option.
    request
        .model_options
        .insert("webSearch".into(), json!("off"));
    request
        .model_options
        .insert("maxTokens".into(), json!(8192));
    let config = configuration(&request, dir.path(), "session").unwrap();
    assert_eq!(config["webSearch"], false);
    assert_eq!(config["maxTokens"], 8192);
    // Anything the models do not advertise is still refused.
    request
        .model_options
        .insert("speed".into(), json!("fast"));
    assert!(configuration(&request, dir.path(), "session").is_err());
}

#[test]
fn host_owned_profile_home_and_git_overlays_are_preserved_without_other_auth() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(dir.path());
    let home = dir.path().join("profile/deepseek");
    request.env = BTreeMap::from([
        ("ZERON_DEEPSEEK_HOME".into(), home.display().to_string()),
        ("DEEPSEEK_API_KEY".into(), "synthetic-deepseek".into()),
        ("GH_TOKEN".into(), "synthetic-github".into()),
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), "core.hooksPath".into()),
        ("GIT_CONFIG_VALUE_0".into(), "/trusted/hooks".into()),
        ("GIT_CONFIG_KEY_8".into(), "untrusted-unused".into()),
        ("OPENAI_API_KEY".into(), "forbidden".into()),
        ("ANTHROPIC_API_KEY".into(), "forbidden".into()),
        ("NODE_OPTIONS".into(), "forbidden".into()),
    ]);
    let config = configuration(&request, dir.path(), "session").unwrap();
    assert_eq!(config["dshHome"], home.display().to_string());
    let environment = launch_environment(&request).unwrap();
    assert_eq!(
        environment
            .get(std::ffi::OsStr::new("DEEPSEEK_API_KEY"))
            .unwrap(),
        "synthetic-deepseek"
    );
    assert_eq!(
        environment.get(std::ffi::OsStr::new("GH_TOKEN")).unwrap(),
        "synthetic-github"
    );
    assert_eq!(
        environment
            .get(std::ffi::OsStr::new("GIT_CONFIG_KEY_0"))
            .unwrap(),
        "core.hooksPath"
    );
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "NODE_OPTIONS",
        "GIT_CONFIG_KEY_8",
        "ZERON_DEEPSEEK_HOME",
    ] {
        assert!(!environment.contains_key(std::ffi::OsStr::new(key)));
    }
    request
        .env
        .insert("ZERON_DEEPSEEK_HOME".into(), "relative/home".into());
    assert!(configuration(&request, dir.path(), "session").is_err());
    request.env.remove("GIT_CONFIG_VALUE_0");
    assert!(launch_environment(&request).is_err());
}

#[test]
fn installed_probe_requires_matching_sdk_runtime_and_protocol_without_running_them() {
    let dir = tempfile::tempdir().unwrap();
    for package in ["dsh-sdk-client", "dsh", "dsh-sdk-protocol"] {
        let root = dir.path().join("node_modules/@deepseek-ai").join(package);
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("package.json"),
            json!({"version":SDK_VERSION}).to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join(if package == "dsh" {
                "lib/bin.js"
            } else {
                "lib/index.js"
            }),
            "throw new Error('must not execute')",
        )
        .unwrap();
    }
    assert!(sdk_installed(dir.path()));
    std::fs::write(
        dir.path()
            .join("node_modules/@deepseek-ai/dsh-sdk-client/package.json"),
        "{\"version\":\"0.0.1-rc.1\"}",
    )
    .unwrap();
    assert!(!sdk_installed(dir.path()));
}

#[tokio::test]
async fn advertises_the_dsh_commands_the_sidecar_can_route() {
    let commands = DeepSeekHarness::new().commands().await.unwrap();
    let names: Vec<&str> = commands.iter().map(|command| command.name.as_str()).collect();
    // `/plan` is deliberately absent: plan mode is set by Zeron when a run
    // starts, and the sidecar refuses a typed `/plan` (shim.mjs), so offering
    // it would put a command in the composer that always errors. `/goal` and
    // `/compact` route through `session/query` and their direct text reaches
    // the transcript (SUR-57).
    assert_eq!(names, ["goal", "compact"]);
    assert_eq!(
        commands[0].description,
        "Create, edit, pause or resume a goal"
    );
    assert_eq!(
        commands[0].input_hint.as_deref(),
        Some("[objective | edit | pause | resume | clear]")
    );
    assert_eq!(
        commands[1].description,
        "Summarize the conversation to free context"
    );
    assert_eq!(commands[1].input_hint, None);
}

#[tokio::test]
#[ignore = "requires Node and the pinned SDK prefix in ZERON_DEEPSEEK_SDK_DIR"]
async fn sdk_runtime_round_trip_and_resume() {
    let prefix = std::env::var_os("ZERON_DEEPSEEK_SDK_DIR").expect("SDK prefix");
    let dir = tempfile::tempdir().unwrap();
    let (base_url, requests) = mock_model().await;
    // The pinned runtime, not the fixture: the plan-mode guidance is the
    // runtime's own `plan:policy` prompt section, so only a real run can show
    // whether a resumed run still carries it.
    let harness = DeepSeekHarness {
        sdk_root: prefix.into(),
        home: dir.path().join("dsh"),
        test_runtime: None,
    };
    let mut run = request(dir.path());
    run.env
        .insert("DEEPSEEK_API_KEY".into(), "synthetic-not-live".into());
    run.env.insert("DEEPSEEK_BASE_URL".into(), base_url);
    // Round 1 enters plan mode, round 2 resumes with the toggle off (the fix
    // under test), and round 3 resumes the now-inactive session, which has to
    // be the success no-op `/plan off` promises rather than a failed start.
    let mut saved = None;
    for (index, prompt) in ["first", "resumed", "resumed again"].into_iter().enumerate() {
        run.prompt = prompt.into();
        run.resume = saved.clone();
        run.plan = index == 0;
        let (_steer_tx, steering) = mpsc::channel(4);
        let interrupt = crate::CancellationToken::new();
        let controls = RunControls {
            execution_lease: None,
            request_input: Box::new(|_| {
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(vec![]);
                rx
            }),
            steering,
            interrupt: interrupt.clone(),
            turn: Default::default(),
        };
        let mut events = harness.run(run.clone(), controls).await.unwrap();
        let mut text = String::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(30), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            match event {
                AgentEvent::SessionStarted { session_id, .. } => {
                    if let Some(saved) = &saved {
                        assert_eq!(saved, &session_id);
                    }
                    saved = Some(session_id);
                }
                AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                AgentEvent::Done { status, error, .. } => {
                    assert_eq!(status, DoneStatus::Completed, "{error:?}");
                    assert_eq!(text, "SDK_RUNTIME_OK");
                    break;
                }
                _ => {}
            }
        }
        interrupt.cancel();
        loop {
            match tokio::time::timeout(Duration::from_secs(10), events.next()).await {
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
    }
    let seen = requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "one model request per round");
    // DeepSeek carries system-prompt updates in history (`systemPromptUpdate:
    // 'in-history'`): the `system` field keeps the session's first snapshot and
    // each change arrives as a later system snapshot in `messages`, so the last
    // one is what the model is told right now.
    let snapshot = |request: &Value| -> String {
        let text = |value: &Value| -> String {
            value["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default()
        };
        if let Some(current) = request["messages"]
            .as_array()
            .and_then(|messages| messages.iter().rev().find(|message| message["role"] == "system"))
        {
            return text(current);
        }
        request["system"].as_str().unwrap_or_default().into()
    };
    let in_plan_mode = |request: &Value| snapshot(request).contains("You are in plan mode");
    assert!(in_plan_mode(&seen[0]), "round 1 entered plan mode");
    assert!(
        !in_plan_mode(&seen[1]),
        "the resumed run left plan mode before its prompt"
    );
    assert!(
        !in_plan_mode(&seen[2]),
        "a second /plan off on an inactive session is a no-op"
    );
}

/// A loopback Anthropic streaming endpoint: it records every request body and
/// answers with one completed text turn, so the pinned runtime can run a real
/// turn with no network and the test can read the guidance it sent.
async fn mock_model() -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move {
                let Some(body) = http_body(&mut socket).await else {
                    return;
                };
                if let Ok(request) = serde_json::from_slice::<Value>(&body) {
                    recorded.lock().unwrap().push(request);
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                            cache-control: no-cache\r\nconnection: close\r\n\r\n";
                let _ = socket
                    .write_all(format!("{head}{MOCK_REPLY}").as_bytes())
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("http://{address}/anthropic"), requests)
}

/// One complete Anthropic SSE text turn, shaped like the node mock's reply.
const MOCK_REPLY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{",
    "\"id\":\"msg-mock\",\"type\":\"message\",\"role\":\"assistant\",",
    "\"model\":\"deepseek-flash\",\"content\":[],\"stop_reason\":null,",
    "\"stop_sequence\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",",
    "\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",",
    "\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"SDK_RUNTIME_OK\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",",
    "\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},",
    "\"usage\":{\"output_tokens\":3}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

/// Read one HTTP request off the socket and return its body, which the model
/// client always sends with a `content-length`.
async fn http_body(socket: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                return Some(data[end + 4..end + 4 + length].to_vec());
            }
        }
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..read]);
    }
}
