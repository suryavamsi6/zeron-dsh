//! DeepSeek credential readiness. The official SDK owns credential files and sign-in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeron_proto::{
    AgentAccount, AgentAccountWarning, AgentAccountsSnapshot, AgentAuthKind, AgentLoginPoll,
    AgentLoginStart, HarnessId,
};

use crate::EngineError;

pub const SETUP_GUIDANCE: &str = "Set DEEPSEEK_API_KEY for the engine or configure credentials with the official DeepSeek harness SDK.";

#[derive(Debug, Clone)]
pub struct AgentAccountsConfig {
    pub data_dir: PathBuf,
    pub deepseek_home: PathBuf,
}

impl AgentAccountsConfig {
    pub fn detect(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            deepseek_home: std::env::var_os("ZERON_DEEPSEEK_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("deepseek")),
        }
    }

    pub fn isolated(root: &Path) -> Self {
        Self {
            data_dir: root.join("data"),
            deepseek_home: root.join("data/deepseek"),
        }
    }
}

/// The env-shaped ref the harness's own key store answers to: the gateway's
/// `credentials/describe` names the same ref for DSH's DeepSeek route, and
/// both must hold the key for a run to have it.
pub const DEEPSEEK_KEY_REF: &str = "DEEPSEEK_API_KEY";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayedCallback {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

pub type PinHook =
    Arc<dyn Fn(HarnessId, String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

#[derive(Clone)]
pub struct AgentAccounts {
    config: Arc<AgentAccountsConfig>,
    snapshots: tokio::sync::watch::Sender<Option<AgentAccountsSnapshot>>,
}

fn unsupported(harness: HarnessId) -> EngineError {
    EngineError::Other(if harness == HarnessId::DeepSeek {
        SETUP_GUIDANCE.into()
    } else {
        "This legacy provider is no longer supported; select DeepSeek. Stored credentials have not been changed.".into()
    })
}

impl AgentAccounts {
    pub fn new(config: AgentAccountsConfig) -> Self {
        Self {
            config: Arc::new(config),
            snapshots: tokio::sync::watch::channel(None).0,
        }
    }

    pub fn with_callback_routes(
        config: AgentAccountsConfig,
        _routes: zeron_preview::login::CallbackRoutes,
    ) -> Self {
        Self::new(config)
    }

    pub fn watch_snapshots(&self) -> tokio::sync::watch::Receiver<Option<AgentAccountsSnapshot>> {
        self.snapshots.subscribe()
    }

    fn key_file(&self) -> PathBuf {
        self.config.deepseek_home.join("api-key")
    }

    fn stored_key(&self) -> Result<Option<String>, EngineError> {
        match std::fs::read_to_string(self.key_file()) {
            Ok(key) if valid_key(&key) => Ok(Some(key)),
            Ok(_) => Err(EngineError::Other(
                "The saved DeepSeek API key is malformed; replace it in Settings.".into(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(EngineError::Other(
                "The saved DeepSeek API key could not be read.".into(),
            )),
        }
    }

    pub async fn list(&self, _force_usage: bool) -> Result<AgentAccountsSnapshot, EngineError> {
        let stored = self.stored_key()?.is_some();
        let ready = stored
            || std::env::var("DEEPSEEK_API_KEY")
                .ok()
                .is_some_and(|key| valid_key(&key));
        let snapshot = AgentAccountsSnapshot {
            accounts: if ready {
                vec![AgentAccount {
                    id: if stored {
                        "deepseek-key"
                    } else {
                        "deepseek-environment"
                    }
                    .into(),
                    harness: HarnessId::DeepSeek,
                    email: None,
                    plan_label: None,
                    active: true,
                    usage_windows: Vec::new(),
                    usage_fetched_at: None,
                    usage_error: None,
                    codex_reset_credits: None,
                    display_name: Some(
                        if stored {
                            "DeepSeek API key"
                        } else {
                            "DEEPSEEK_API_KEY"
                        }
                        .into(),
                    ),
                    organization: None,
                    auth_kind: Some(AgentAuthKind::ApiKey),
                    switchable: false,
                    saved_at: None,
                    provider: Some("deepseek-official".into()),
                }]
            } else {
                Vec::new()
            },
            warnings: if ready {
                Vec::new()
            } else {
                vec![AgentAccountWarning {
                    harness: HarnessId::DeepSeek,
                    message: SETUP_GUIDANCE.into(),
                }]
            },
        };
        self.snapshots.send_if_modified(|current| {
            if current.as_ref() == Some(&snapshot) {
                return false;
            }
            *current = Some(snapshot.clone());
            true
        });
        Ok(snapshot)
    }

    pub async fn activate(
        &self,
        harness: HarnessId,
        _account_id: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        Err(unsupported(harness))
    }

    pub async fn forget(
        &self,
        harness: HarnessId,
        account_id: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        if harness != HarnessId::DeepSeek || account_id != "deepseek-key" {
            return Err(unsupported(harness));
        }
        match std::fs::remove_file(self.key_file()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(EngineError::Other(
                    "The saved DeepSeek API key could not be removed.".into(),
                ));
            }
        }
        self.list(false).await
    }

    pub async fn add_api_key(
        &self,
        harness: HarnessId,
        provider: &str,
        key: &str,
        _name: Option<&str>,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        if harness != HarnessId::DeepSeek || provider != "deepseek" {
            return Err(unsupported(harness));
        }
        if !valid_key(key) {
            return Err(EngineError::Other("The DeepSeek API key must be nonempty and contain no whitespace or control characters.".into()));
        }
        private_dir(&self.config.deepseek_home)?;
        crate::runner::write_private_atomic(&self.key_file(), key.as_bytes())?;
        self.list(false).await
    }

    pub async fn rename_account(
        &self,
        harness: HarnessId,
        _account_id: &str,
        _name: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        Err(unsupported(harness))
    }

    pub async fn start_login(&self, harness: HarnessId) -> Result<AgentLoginStart, EngineError> {
        Err(unsupported(harness))
    }

    pub async fn start_login_with(
        &self,
        harness: HarnessId,
        _provider: Option<&str>,
        _requester: Option<&str>,
    ) -> Result<AgentLoginStart, EngineError> {
        Err(unsupported(harness))
    }

    pub async fn complete_login(
        &self,
        _login_id: &str,
        _code: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        Err(EngineError::Other(SETUP_GUIDANCE.into()))
    }

    pub async fn poll_login(&self, _login_id: &str) -> Result<AgentLoginPoll, EngineError> {
        Err(EngineError::Other(SETUP_GUIDANCE.into()))
    }

    pub async fn relay_callback(
        &self,
        _login_id: &str,
        _requester: &str,
        _path: &str,
    ) -> Result<RelayedCallback, EngineError> {
        Err(EngineError::Other(
            "DeepSeek does not use legacy browser sign-in callbacks.".into(),
        ))
    }

    pub fn cancel_login(&self, _login_id: &str) {}
    pub fn set_pin_hook(&self, _hook: PinHook) {}
    pub fn shutdown(&self) {}
}

pub(crate) fn tunnel_port_allowed(_port: u16, _url: Option<&str>) -> bool {
    false
}

pub struct PinRunEnvironment(pub AgentAccounts);

#[async_trait::async_trait]
impl crate::registry::RunEnvironment for PinRunEnvironment {
    async fn prepare(
        &self,
        harness: HarnessId,
        chat_id: Option<&str>,
        request: &mut zeron_proto::RunRequest,
    ) -> Result<(), String> {
        if !matches!(harness, HarnessId::DeepSeek | HarnessId::Mock) {
            return Err(unsupported(harness).to_string());
        }
        if harness == HarnessId::Mock {
            return Ok(());
        }
        request.env.insert(
            "ZERON_DEEPSEEK_HOME".into(),
            self.0.config.deepseek_home.to_string_lossy().into_owned(),
        );
        // The Captain role this chat plays rides `env` (host-only, never the
        // wire or the prompt), and the same single load provisions its Drive:
        // a Captain or crew chat's first dispatch creates it (create-if-absent,
        // so an edit survives a retry), and a failure refuses the run rather
        // than letting it start half-provisioned. A missing or unreadable
        // captains.json simply leaves the run unstamped and unprovisioned —
        // ordinary chats must still prepare, and an ordinary chat creates
        // nothing.
        if let Some(chat_id) = chat_id
            && let Ok(registry) = crate::captains::Registry::load(&self.0.config.data_dir)
        {
            if let Some(value) =
                crate::captains::captains_env(&registry, &self.0.config.deepseek_home, chat_id)
            {
                request.env.insert("ZERON_DEEPSEEK_CAPTAINS".into(), value);
            }
            crate::captains::ensure_drive(&registry, &self.0.config.deepseek_home, chat_id)?;
        }
        // Zeron's own Harness home is the one credential store: the gateway
        // writes it and every run reads it, so a provider configured in the
        // app is the provider the next run uses. A run that must not see the
        // store leaves this unset and gets a fenced per-run file instead.
        request
            .env
            .insert("ZERON_DEEPSEEK_SHARED_HOME".into(), "1".into());
        if !request.env.contains_key("DEEPSEEK_API_KEY")
            && let Some(key) = self.0.stored_key().map_err(|error| error.to_string())?
        {
            request.env.entry("DEEPSEEK_API_KEY".into()).or_insert(key);
        }
        Ok(())
    }

    async fn credential(&self, harness: HarnessId, _chat_id: Option<&str>) -> Option<String> {
        if harness != HarnessId::DeepSeek {
            return None;
        }
        let key = self.0.stored_key().ok().flatten().or_else(|| {
            std::env::var("DEEPSEEK_API_KEY")
                .ok()
                .filter(|key| valid_key(key))
        });
        Some(key.map_or_else(
            || "deepseek-sdk".into(),
            |key| format!("deepseek-key:{:x}", Sha256::digest(key.as_bytes())),
        ))
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 512 && !key.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RunEnvironment;

    #[tokio::test]
    async fn legacy_requests_leave_profiles_and_logins_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig::isolated(root.path());
        let saved = config.data_dir.join("agent-accounts/claude-code/old.json");
        std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
        std::fs::write(&saved, b"historical credentials").unwrap();
        let pins = config.data_dir.join("agent-accounts/pins.json");
        std::fs::write(&pins, b"historical pins").unwrap();
        let accounts = AgentAccounts::new(config);
        let legacy = HarnessId::ClaudeCode;
        assert!(accounts.activate(legacy, "old").await.is_err());
        assert!(accounts.forget(legacy, "old").await.is_err());
        let error = accounts
            .add_api_key(legacy, "anthropic", "never-echo-this-key", None)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("never-echo-this-key"));
        assert!(accounts.start_login(legacy).await.is_err());
        assert!(accounts.rename_account(legacy, "old", "new").await.is_err());
        let snapshot = accounts.list(true).await.unwrap();
        assert!(
            snapshot
                .accounts
                .iter()
                .all(|a| a.harness == HarnessId::DeepSeek)
        );
        assert_eq!(std::fs::read(&saved).unwrap(), b"historical credentials");
        assert_eq!(std::fs::read(&pins).unwrap(), b"historical pins");
    }

    #[tokio::test]
    async fn construction_and_listing_do_not_create_profile_storage() {
        let root = tempfile::tempdir().unwrap();
        let accounts = AgentAccounts::new(AgentAccountsConfig::isolated(root.path()));
        accounts.list(true).await.unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert!(!tunnel_port_allowed(1455, Some("https://example.org")));
    }

    #[tokio::test]
    async fn snapshots_publish_only_when_readiness_changes() {
        let accounts = AgentAccounts::new(AgentAccountsConfig::isolated(Path::new("unused")));
        let mut watch = accounts.watch_snapshots();
        accounts.list(false).await.unwrap();
        assert!(watch.has_changed().unwrap());
        watch.borrow_and_update();
        accounts.list(true).await.unwrap();
        assert!(!watch.has_changed().unwrap());
    }

    #[tokio::test]
    async fn saved_keys_reach_only_host_environment_and_leave_sdk_configuration_untouched() {
        let root = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig::isolated(root.path());
        std::fs::create_dir_all(&config.deepseek_home).unwrap();
        let sdk_config = config.deepseek_home.join("config.json");
        std::fs::write(&sdk_config, b"preserve SDK configuration").unwrap();
        let accounts = AgentAccounts::new(config.clone());
        let key = "deepseek-local-test-fixture";
        let snapshot = accounts
            .add_api_key(HarnessId::DeepSeek, "deepseek", key, None)
            .await
            .unwrap();
        assert_eq!(snapshot.accounts[0].id, "deepseek-key");
        assert!(!serde_json::to_string(&snapshot).unwrap().contains(key));
        let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .unwrap();
        let env = PinRunEnvironment(accounts.clone());
        env.prepare(HarnessId::DeepSeek, None, &mut request)
            .await
            .unwrap();
        assert_eq!(request.env["DEEPSEEK_API_KEY"], key);
        assert_eq!(
            request.env["ZERON_DEEPSEEK_HOME"],
            config.deepseek_home.to_string_lossy()
        );
        // Every run the host drives reads Zeron's own store, so a provider
        // configured in the app is the provider the next run uses.
        assert_eq!(request.env["ZERON_DEEPSEEK_SHARED_HOME"], "1");
        assert!(!serde_json::to_string(&request).unwrap().contains(key));
        assert!(!format!("{request:?}").contains(key));
        let fingerprint = env.credential(HarnessId::DeepSeek, None).await.unwrap();
        assert!(!fingerprint.contains(key));
        request
            .env
            .insert("DEEPSEEK_API_KEY".into(), "broker-key".into());
        env.prepare(HarnessId::DeepSeek, None, &mut request)
            .await
            .unwrap();
        assert_eq!(request.env["DEEPSEEK_API_KEY"], "broker-key");
        let before = request.env.clone();
        assert!(
            env.prepare(HarnessId::Codex, None, &mut request)
                .await
                .is_err()
        );
        assert_eq!(request.env, before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(accounts.key_file())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&config.deepseek_home)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert_eq!(
            std::fs::read(&sdk_config).unwrap(),
            b"preserve SDK configuration"
        );
        accounts
            .forget(HarnessId::DeepSeek, "deepseek-key")
            .await
            .unwrap();
        assert!(!accounts.key_file().exists());
        assert!(sdk_config.exists());
    }

    #[tokio::test]
    async fn the_test_harness_needs_no_credential_and_gets_no_environment() {
        let root = tempfile::tempdir().unwrap();
        let accounts = AgentAccounts::new(AgentAccountsConfig::isolated(root.path()));
        let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .unwrap();
        let env = PinRunEnvironment(accounts.clone());
        env.prepare(HarnessId::Mock, None, &mut request)
            .await
            .unwrap();
        assert!(request.env.is_empty());
        assert!(env.credential(HarnessId::Mock, None).await.is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn invalid_keys_and_providers_never_create_or_rewrite_storage() {
        let root = tempfile::tempdir().unwrap();
        let accounts = AgentAccounts::new(AgentAccountsConfig::isolated(root.path()));
        for key in ["", " padded", "bad key", "bad\nkey", "bad\u{7f}key"] {
            assert!(
                accounts
                    .add_api_key(HarnessId::DeepSeek, "deepseek", key, None)
                    .await
                    .is_err()
            );
        }
        assert!(
            accounts
                .add_api_key(HarnessId::DeepSeek, "openai", "valid-fixture", None)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn stamp_follows_role_changes_and_stays_out_of_prompt() {
        async fn prepare(
            env: &PinRunEnvironment,
            chat_id: Option<&str>,
        ) -> zeron_proto::RunRequest {
            let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
                "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
            }))
            .unwrap();
            env.prepare(HarnessId::DeepSeek, chat_id, &mut request)
                .await
                .unwrap();
            request
        }

        let temp = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig {
            data_dir: temp.path().join("data"),
            deepseek_home: temp.path().join("deepseek"),
        };
        let env = PinRunEnvironment(AgentAccounts::new(config.clone()));
        let store = crate::captains::CaptainsStore::open(config.data_dir.clone()).unwrap();

        // An ordinary chat — nothing on record — runs unstamped, whether the
        // dispatch passes a chat id at all.
        for chat_id in [None, Some("stranger")] {
            let request = prepare(&env, chat_id).await;
            assert!(!request.env.contains_key("ZERON_DEEPSEEK_CAPTAINS"));
        }

        store.promote("chat-1", "ada").unwrap();
        let request = prepare(&env, Some("chat-1")).await;
        let stamp: serde_json::Value =
            serde_json::from_str(&request.env["ZERON_DEEPSEEK_CAPTAINS"]).unwrap();
        assert_eq!(stamp["role"], "captain");
        assert_eq!(stamp["chatId"], "chat-1");
        assert_eq!(stamp["captainId"], "chat-1");
        assert_eq!(stamp["captainName"], "ada");
        assert_eq!(
            stamp["driveRoot"],
            crate::captains::drive_dir(&config.deepseek_home, "chat-1")
                .to_string_lossy()
                .as_ref()
        );

        store.add_crew("chat-1", "chat-2").unwrap();
        let request = prepare(&env, Some("chat-2")).await;
        let stamp: serde_json::Value =
            serde_json::from_str(&request.env["ZERON_DEEPSEEK_CAPTAINS"]).unwrap();
        assert_eq!(stamp["role"], "crew");
        assert_eq!(stamp["chatId"], "chat-2");
        assert_eq!(stamp["captainId"], "chat-1");
        assert_eq!(stamp["captainName"], "ada");

        // Demoting archives the Captain: the next run releases it and its
        // crew — nothing about the run keeps the old role.
        store.demote("chat-1").unwrap();
        for chat_id in ["chat-1", "chat-2"] {
            let request = prepare(&env, Some(chat_id)).await;
            assert!(
                !request.env.contains_key("ZERON_DEEPSEEK_CAPTAINS"),
                "{chat_id} is released on the next run"
            );
        }

        // The stamp is host-only: it rides `env`, which is serde-skipped on
        // the wire, and never lands in text a child would read.
        store.promote("chat-1", "ada").unwrap();
        let request = prepare(&env, Some("chat-1")).await;
        assert!(request.env.contains_key("ZERON_DEEPSEEK_CAPTAINS"));
        assert!(!request.prompt.contains("ZERON_DEEPSEEK_CAPTAINS"));
        assert!(!request.prompt.contains("driveRoot"));
        let wire = serde_json::to_string(&request).unwrap();
        assert!(!wire.contains("ZERON_DEEPSEEK_CAPTAINS"));
        assert!(!wire.contains("driveRoot"));
    }

    #[tokio::test]
    async fn dispatch_prepares_the_captains_drive() {
        let root = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig {
            data_dir: root.path().join("data"),
            deepseek_home: root.path().join("deepseek"),
        };
        let store = crate::captains::CaptainsStore::open(config.data_dir.clone()).unwrap();
        store.promote("cap-a", "ada").unwrap();

        let env = PinRunEnvironment(AgentAccounts::new(config.clone()));
        let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .unwrap();
        env.prepare(HarnessId::DeepSeek, Some("cap-a"), &mut request)
            .await
            .unwrap();

        let drive = config.deepseek_home.join("captains/cap-a");
        for file in ["AGENTS.md", "CREW.md", "NOTES.md", "DIGEST.md"] {
            assert!(drive.join(file).is_file(), "the drive lost {file}");
        }
        for dir in ["skills", "reports", "renders", "handoffs", "state"] {
            assert!(drive.join(dir).is_dir(), "the drive lost {dir}/");
        }

        let mut ordinary: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .unwrap();
        env.prepare(HarnessId::DeepSeek, Some("chat-x"), &mut ordinary)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_dir(config.deepseek_home.join("captains"))
                .unwrap()
                .count(),
            1,
            "an ordinary chat grew a second drive"
        );
    }

    #[tokio::test]
    async fn a_malformed_registry_still_prepares_an_ordinary_chat() {
        let root = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig {
            data_dir: root.path().join("data"),
            deepseek_home: root.path().join("deepseek"),
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        std::fs::write(config.data_dir.join("captains.json"), "{ not json").unwrap();
        let env = PinRunEnvironment(AgentAccounts::new(config.clone()));
        let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .unwrap();

        env.prepare(HarnessId::DeepSeek, Some("chat-x"), &mut request)
            .await
            .expect("an unreadable roster must not refuse an ordinary chat");
        assert!(!request.env.contains_key("ZERON_DEEPSEEK_CAPTAINS"));
        assert!(
            !config.deepseek_home.exists(),
            "an ordinary chat creates nothing, not even the captains root"
        );
    }

    #[tokio::test]
    async fn a_failed_drive_provisioning_refuses_the_run() {
        let root = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig {
            data_dir: root.path().join("data"),
            deepseek_home: root.path().join("deepseek"),
        };
        let store = crate::captains::CaptainsStore::open(config.data_dir.clone()).unwrap();
        store.promote("cap-a", "ada").unwrap();
        std::fs::create_dir_all(&config.deepseek_home).unwrap();
        // A file where the captains root must be makes every drive write fail.
        std::fs::write(config.deepseek_home.join("captains"), "not a directory").unwrap();
        let env = PinRunEnvironment(AgentAccounts::new(config.clone()));
        let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .unwrap();

        let refused = env
            .prepare(HarnessId::DeepSeek, Some("cap-a"), &mut request)
            .await;
        let error = refused.expect_err("a Captain must not run half-provisioned");
        assert!(error.contains("cannot create"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn a_registry_row_the_plugin_would_refuse_is_not_stamped() {
        let root = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig {
            data_dir: root.path().join("data"),
            deepseek_home: root.path().join("deepseek"),
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        // A hand-written row, not one this engine promoted: an invalid id and
        // a name outside the plugin's 1-80/no-CR-LF rule.
        let long_name = "x".repeat(81);
        std::fs::write(
            config.data_dir.join("captains.json"),
            format!(
                r#"{{"captains":[
                    {{"chatId":"bad/id","name":"Ada","createdAt":"2026-11-01T00:00:00Z","archived":false,"crew":[]}},
                    {{"chatId":"cap-long","name":{long_name:?},"createdAt":"2026-11-01T00:00:00Z","archived":false,"crew":[]}}
                ]}}"#
            ),
        )
        .unwrap();
        let env = PinRunEnvironment(AgentAccounts::new(config.clone()));
        for chat_id in ["bad/id", "cap-long"] {
            let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
                "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
            }))
            .unwrap();
            env.prepare(HarnessId::DeepSeek, Some(chat_id), &mut request)
                .await
                .unwrap_or_else(|error| panic!("{chat_id} must run ordinary: {error}"));
            assert!(
                !request.env.contains_key("ZERON_DEEPSEEK_CAPTAINS"),
                "{chat_id} was stamped although the plugin would refuse the row"
            );
        }
        assert!(
            !config.deepseek_home.exists(),
            "a refused row created something on disk"
        );
    }

    /// SUR-191: the dispatch path already tolerates a malformed registry — the
    /// load is an `if let Ok`, so a run proceeds unstamped and creates nothing.
    /// This is the half the issue flagged unverified, and the half that shows
    /// startup dying on the same file is the two paths disagreeing. Expected
    /// green before the startup fix: it pins the `Ok` side of the property.
    #[tokio::test]
    async fn a_malformed_roster_leaves_a_run_unstamped() {
        let root = tempfile::tempdir().unwrap();
        let config = AgentAccountsConfig {
            data_dir: root.path().join("data"),
            deepseek_home: root.path().join("deepseek"),
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        std::fs::write(
            config.data_dir.join("captains.json"),
            b"{\"captains\": [",
        )
        .unwrap();
        let env = PinRunEnvironment(AgentAccounts::new(config.clone()));
        let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .unwrap();
        env.prepare(HarnessId::DeepSeek, Some("cap-a"), &mut request)
            .await
            .expect("a malformed roster must not refuse an ordinary run");
        assert!(
            !request.env.contains_key("ZERON_DEEPSEEK_CAPTAINS"),
            "a malformed roster stamped a role"
        );
        assert!(
            !config.deepseek_home.exists(),
            "a malformed roster created something on disk"
        );
    }
}