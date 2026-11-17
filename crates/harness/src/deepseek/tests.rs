use super::*;
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
    request.reasoning = Some(ReasoningLevel::Minimal);
    assert!(configuration(&request, dir.path(), "session").is_err());
    request.reasoning = Some(ReasoningLevel::Max);
    assert_eq!(
        configuration(&request, dir.path(), "session").unwrap()["reasoningEffort"],
        "max"
    );
    assert!(!crate::supports_titles(HarnessId::DeepSeek));
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
#[ignore = "requires Node and the pinned SDK prefix in ZERON_DEEPSEEK_SDK_DIR"]
async fn sdk_runtime_round_trip_and_resume() {
    let prefix = std::env::var_os("ZERON_DEEPSEEK_SDK_DIR").expect("SDK prefix");
    let dir = tempfile::tempdir().unwrap();
    let harness = DeepSeekHarness {
        sdk_root: prefix.into(),
        home: dir.path().join("dsh"),
        test_runtime: Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deepseek-runtime.mjs"),
        ),
    };
    let mut run = request(dir.path());
    run.env
        .insert("DEEPSEEK_API_KEY".into(), "synthetic-not-live".into());
    let mut saved = None;
    for prompt in ["first", "resumed"] {
        run.prompt = prompt.into();
        run.resume = saved.clone();
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
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
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
                    assert_eq!(text, format!("reply:{prompt}"));
                    break;
                }
                _ => {}
            }
        }
        interrupt.cancel();
        while tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .is_some()
        {}
    }
}
