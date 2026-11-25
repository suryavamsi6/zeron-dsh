//! DSH's app gateway — the surface DSH Web drives — reached headlessly.
//!
//! `dsh web` prints an authenticated URL. Opening it once answers 303 with the
//! signed browser cookie, and `POST /api/<namespace>/<method>` then carries
//! `{type: "client-request", rpcId, method, payload: {args}}` to the mounted
//! controllers (settings, credentials, llm, account). Both cookie flavours
//! resolve to the owner principal, so a loopback request is unrestricted by
//! the remote-access policy; the fence that matters is the launch token the
//! service prints on its own stdout.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};

/// The line `dsh web` prints before it serves anything.
const LAUNCH_PREFIX: &str = "dsh web: ";
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Zeron's own ChatGPT sign-in plugin, loaded into the gateway's profile.
///
/// DSH's pi-ai adapter refuses that flow on purpose (`UNSERVED_OAUTH` holds
/// `openai`, and `models.login` gets no `LoginOptions`), and the API-key flow
/// already owns `llm-pi-ai/openai`, so a plugin cannot register a flow there
/// either. This one runs pi-ai's login itself and lets it persist the grant
/// through the credential seam, which is the only route the shipped adapter
/// leaves open.
const CHATGPT_PLUGIN: &str = include_str!("chatgpt.mjs");
const CHATGPT_ACCOUNTS: &str = include_str!("chatgpt-accounts.mjs");
const PROVIDER_AUTH_PLUGIN: &str = include_str!("provider-auth.mjs");

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("the DeepSeek Harness web surface is not installed: {0}")]
    NotInstalled(String),
    #[error("could not start the DeepSeek Harness settings service: {0}")]
    Start(String),
    #[error("the settings service did not answer: {0}")]
    Transport(String),
    #[error("{code}: {message}")]
    Refused { code: String, message: String },
}

/// One running `dsh web`: where it listens and the cookie that admits calls.
struct Session {
    base: String,
    cookie: String,
    child: Child,
    dotenv: BTreeMap<String, String>,
}

/// The gateway over one DSH home. Its settings and credentials are the same
/// store a session spawned against that home reads, so a write here is what
/// the runtime uses next turn.
pub struct Gateway {
    sdk_root: PathBuf,
    home: PathBuf,
    http: reqwest::Client,
    session: Mutex<Option<Session>>,
}

impl Gateway {
    pub fn new(sdk_root: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            sdk_root: sdk_root.into(),
            home: home.into(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(CALL_TIMEOUT)
                .build()
                .unwrap_or_default(),
            session: Mutex::new(None),
        }
    }

    /// The address a service that may have to be started answers on: a call
    /// that needs a callback origin (a sign-in) asks for it before it calls.
    pub async fn origin(&self) -> Result<String, GatewayError> {
        Ok(self.endpoint().await?.0)
    }

    /// Call one gateway method with its argument object.
    pub async fn call(&self, method: &str, args: Value) -> Result<Value, GatewayError> {
        let (base, cookie) = self.endpoint().await?;
        let body = request_body(method, args);
        let response = self
            .http
            .post(format!("{base}/api/{method}"))
            .header(reqwest::header::COOKIE, cookie)
            .json(&body)
            .send()
            .await
            .map_err(|error| GatewayError::Transport(error.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| GatewayError::Transport(error.to_string()))?;
        interpret(status.as_u16(), &text)
    }

    /// A credential value from the `.env` snapshot this gateway launched
    /// with. DSH reads those files once while booting, so a later edit must
    /// not make the account read answer for a different key than the running
    /// gateway reports configured.
    pub async fn dotenv_key(&self, reference: &str) -> Option<String> {
        self.endpoint().await.ok()?;
        self.session
            .lock()
            .await
            .as_ref()?
            .dotenv
            .get(reference)
            .filter(|value| !value.is_empty())
            .cloned()
    }

    /// Stop the service if it is running; the next call starts a fresh one.
    pub async fn shutdown(&self) {
        let mut guard = self.session.lock().await;
        if let Some(mut session) = guard.take() {
            let _ = session.child.kill().await;
        }
    }

    /// The running service's address and cookie, starting one when needed.
    async fn endpoint(&self) -> Result<(String, String), GatewayError> {
        let mut guard = self.session.lock().await;
        if guard
            .as_mut()
            .is_some_and(|session| !matches!(session.child.try_wait(), Ok(None)))
        {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(self.spawn().await?);
        }
        let session = guard.as_ref().expect("spawned above");
        Ok((session.base.clone(), session.cookie.clone()))
    }

    fn bin(&self) -> PathBuf {
        self.sdk_root
            .join("node_modules/@deepseek-ai/dsh/lib/bin.js")
    }

    /// Write the ChatGPT plugin and the patch that inserts it, and answer the
    /// patch path. The plugin is written beside the home rather than shipped
    /// as a package: it is Zeron's own, and it must load from the same tree
    /// the gateway resolves pi-ai from.
    async fn chatgpt_patch(&self) -> Result<PathBuf, GatewayError> {
        let directory = self.home.join(".zeron-gateway");
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| GatewayError::Start(error.to_string()))?;
        let plugin = directory.join("chatgpt.mjs");
        tokio::fs::write(&plugin, CHATGPT_PLUGIN)
            .await
            .map_err(|error| GatewayError::Start(error.to_string()))?;
        tokio::fs::write(directory.join("chatgpt-accounts.mjs"), CHATGPT_ACCOUNTS)
            .await
            .map_err(|error| GatewayError::Start(error.to_string()))?;
        let provider_auth = directory.join("provider-auth.mjs");
        tokio::fs::write(&provider_auth, PROVIDER_AUTH_PLUGIN)
            .await
            .map_err(|error| GatewayError::Start(error.to_string()))?;
        let patch = directory.join("chatgpt.patch.yml");
        let body = format!(
            "- insert:\n    - id: zeron-chatgpt\n      name: {}\n      config:\n        home: {}\n        sdkRoot: {}\n    - id: zeron-provider-auth\n      name: {}\n",
            quoted(&file_url(&plugin)),
            quoted(&self.home.to_string_lossy()),
            quoted(&self.sdk_root.to_string_lossy()),
            quoted(&file_url(&provider_auth)),
        );
        tokio::fs::write(&patch, body)
            .await
            .map_err(|error| GatewayError::Start(error.to_string()))?;
        Ok(patch)
    }

    async fn spawn(&self) -> Result<Session, GatewayError> {
        let bin = self.bin();
        if !bin.is_file() {
            return Err(GatewayError::NotInstalled(bin.display().to_string()));
        }
        let node = crate::executable::find_on_paths_with(
            "node",
            vec![],
            &|name| std::env::var_os(name),
            crate::shell_env::user_path(),
            crate::executable::Platform::current(),
        )
        .ok_or_else(|| GatewayError::NotInstalled("Node >=24.9".into()))?;
        let dotenv = dotenv_snapshot(&self.home);
        let mut command = Command::new(&node);
        command
            .arg(&bin)
            .arg("--profile")
            .arg("web")
            .arg("--patch")
            .arg(self.chatgpt_patch().await?)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0")
            .arg("--no-open")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in gateway_environment(&self.home) {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|error| GatewayError::Start(error.to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| GatewayError::Start("the service took no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        let launched = tokio::time::timeout(LAUNCH_TIMEOUT, async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(launched) = launched_from(&line) {
                    return Some(launched);
                }
            }
            None
        })
        .await;
        let Some(Some(launched)) = launched.ok() else {
            let _ = child.kill().await;
            return Err(GatewayError::Start(
                "the service never announced its address".into(),
            ));
        };
        let response = self
            .http
            .get(&launched.token_url)
            .send()
            .await
            .map_err(|error| {
                GatewayError::Start(format!("minting the session cookie failed: {error}"))
            })?;
        let cookie = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(cookie_pair);
        let Some(cookie) = cookie else {
            let _ = child.kill().await;
            return Err(GatewayError::Start(
                "the service issued no session cookie".into(),
            ));
        };
        Ok(Session {
            base: launched.base,
            cookie,
            child,
            dotenv,
        })
    }
}

/// The address and token URL in a `dsh web: <url>?token=…` launch line.
struct Launch {
    base: String,
    token_url: String,
}

fn launched_from(line: &str) -> Option<Launch> {
    let url = line.trim().strip_prefix(LAUNCH_PREFIX)?;
    let token = url.split_once("?token=")?.1;
    if token.is_empty() {
        return None;
    }
    let base = url.split('?').next()?.trim_end_matches('/').to_owned();
    if !base.starts_with("http://127.0.0.1:") && !base.starts_with("http://[::1]:") {
        return None;
    }
    Some(Launch {
        base,
        token_url: url.trim().to_owned(),
    })
}

/// `name=value` from one Set-Cookie header, dropping its attributes.
fn cookie_pair(header: &str) -> Option<String> {
    let pair = header.split(';').next()?.trim();
    let (name, value) = pair.split_once('=')?;
    if name.is_empty() || value.is_empty() {
        return None;
    }
    Some(format!("{name}={value}"))
}

fn request_body(method: &str, args: Value) -> Value {
    json!({
        "type": "client-request",
        "rpcId": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "payload": { "args": args },
    })
}

/// The value a gateway answer carries, or the refusal it reports.
fn interpret(status: u16, body: &str) -> Result<Value, GatewayError> {
    let frame: Value = serde_json::from_str(body)
        .map_err(|error| GatewayError::Transport(format!("HTTP {status}: {error}")))?;
    let Some(result) = frame.get("result") else {
        return Err(GatewayError::Transport(format!(
            "HTTP {status}: answer without a result"
        )));
    };
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(result.get("value").cloned().unwrap_or(Value::Null));
    }
    let error = result.get("error");
    Err(GatewayError::Refused {
        code: error
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("gateway/unknown")
            .to_owned(),
        message: error
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("the settings service refused the call")
            .to_owned(),
    })
}

/// The invoking project's `.env` backed by the Harness home's, in the
/// precedence order DSH's launch environment gives them. Values are captured
/// beside the spawn because the child snapshots both files once at boot;
/// within one file a repeated assignment parses the same way Node's object
/// materialization does, with the last value winning.
fn dotenv_snapshot(home: &Path) -> BTreeMap<String, String> {
    let project = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
    dotenv_snapshot_at(home, &project)
}

fn dotenv_snapshot_at(home: &Path, project: &Path) -> BTreeMap<String, String> {
    let mut values = dotenv_file(&project.join(".env"));
    if project != home {
        for (key, value) in dotenv_file(&home.join(".env")) {
            values.entry(key).or_insert(value);
        }
    }
    values
}

fn dotenv_file(path: &Path) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let Ok(body) = std::fs::read_to_string(path) else {
        return values;
    };
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export").map(str::trim_start).unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let mut value = value.trim().to_string();
        if let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'' | '`'))
            && !value[1..].contains(quote)
        {
            while let Some(next) = lines.next() {
                value.push('\n');
                value.push_str(next);
                if next.contains(quote) {
                    break;
                }
            }
        }
        values.insert(key.to_string(), dotenv_value(&value));
    }
    values
}

/// A value in Node `parseEnv`'s line grammar: a quoted value ends at its
/// closing quote; an unquoted value ends at `#`; both trim outer space.
fn dotenv_value(value: &str) -> String {
    if let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'' | '`')) {
        if let Some(end) = value[1..].find(quote) {
            return value[1..1 + end].to_string();
        }
        return value.to_string();
    }
    value.split('#').next().unwrap_or("").trim().to_string()
}

/// What a service process inherits: enough to find its stores and its own
/// tools, nothing of the app's environment that could change its behaviour.
/// A path as the `file:` URL a DSH profile row names a plugin by.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A YAML scalar holding `value` verbatim, whatever it contains.
fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

fn gateway_environment(home: &Path) -> BTreeMap<OsString, OsString> {
    let allowed = [
        "PATH",
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "SystemRoot",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "TMPDIR",
        "TEMP",
        "TMP",
    ];
    let mut environment: BTreeMap<OsString, OsString> = allowed
        .into_iter()
        .filter_map(|key| {
            std::env::var_os(key)
                .filter(|value| !value.is_empty())
                .map(|value| (OsString::from(key), value))
        })
        .collect();
    environment.insert("DSH_HOME".into(), home.as_os_str().to_owned());
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_line_yields_its_loopback_address_and_token_url() {
        let line = "dsh web: http://127.0.0.1:34879/?token=pzMHvHEmVd_EBfOCbjKF\n";
        let launched = launched_from(line).expect("a launch line");
        assert_eq!(launched.base, "http://127.0.0.1:34879");
        assert_eq!(
            launched.token_url,
            "http://127.0.0.1:34879/?token=pzMHvHEmVd_EBfOCbjKF"
        );
    }

    #[test]
    fn anything_but_a_loopback_launch_line_is_ignored() {
        assert!(launched_from("deepseek account: signed out").is_none());
        assert!(launched_from("dsh web: http://10.0.0.7:8080/?token=t").is_none());
        assert!(launched_from("dsh web: http://127.0.0.1:8080/").is_none());
        assert!(launched_from("dsh web: http://127.0.0.1:8080/?token=").is_none());
    }

    #[test]
    fn a_set_cookie_header_keeps_only_its_name_and_value() {
        let header = "dsh-auth-Sl=v1.eyJ2ZXJzaW9uIjoxfQ.ag3y; Max-Age=2592000; Path=/; \
                      Expires=Wed, 16 Dec 2026 16:39:09 GMT; HttpOnly; SameSite=Strict";
        assert_eq!(
            cookie_pair(header).as_deref(),
            Some("dsh-auth-Sl=v1.eyJ2ZXJzaW9uIjoxfQ.ag3y")
        );
        assert!(cookie_pair("HttpOnly").is_none(), "an attribute is not a cookie");
        assert!(cookie_pair("dsh-auth-x=").is_none(), "an empty cookie admits nothing");
    }

    #[test]
    fn a_call_carries_its_method_and_one_object_of_arguments() {
        let body = request_body("llm/listProviders", json!({ "only": "configured" }));
        assert_eq!(body["type"], "client-request");
        assert_eq!(body["method"], "llm/listProviders");
        assert_eq!(body["payload"]["args"]["only"], "configured");
        assert!(body["payload"]["args"].is_object());
        assert!(
            body["rpcId"].as_str().is_some_and(|id| !id.is_empty()),
            "every call is identified"
        );
    }

    #[test]
    fn an_answer_reads_back_its_value_or_its_refusal() {
        let ok = r#"{"type":"server-response","rpcId":"1","result":{"ok":true,"value":{"a":1}}}"#;
        assert_eq!(interpret(200, ok).unwrap()["a"], 1);
        let refused = r#"{"type":"server-response","rpcId":"1","result":{"ok":false,"error":{"code":"gateway/arguments-invalid","message":"missing refs"}}}"#;
        let error = interpret(200, refused).unwrap_err().to_string();
        assert!(error.contains("gateway/arguments-invalid"), "{error}");
        assert!(error.contains("missing refs"), "{error}");
        assert!(interpret(500, "<html>nope</html>").is_err());
    }

    #[test]
    fn the_dotenv_snapshot_is_project_then_home_last_duplicate_and_frozen() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(".env"),
            "MULTILINE=\"one\nMARKER_ACCOUNT_API_KEY=embedded\nend\"\nMARKER_ACCOUNT_API_KEY=first\nMARKER_ACCOUNT_API_KEY=last\nSPACED_KEY=some words\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".env"),
            "MARKER_ACCOUNT_API_KEY=home\nHOME_ONLY_KEY=home-only\n",
        )
        .unwrap();

        let snapshot = dotenv_snapshot_at(&home, &project);
        assert_eq!(
            snapshot.get("MARKER_ACCOUNT_API_KEY").map(String::as_str),
            Some("last"),
            "a repeated assignment resolves to Node parseEnv's last value"
        );
        assert_eq!(
            snapshot.get("MULTILINE").map(String::as_str),
            Some("one\nMARKER_ACCOUNT_API_KEY=embedded\nend"),
            "an assignment inside a quoted multiline value is text"
        );
        assert_eq!(
            snapshot.get("SPACED_KEY").map(String::as_str),
            Some("some words"),
            "an unquoted value keeps its spaces as Node parseEnv reads it"
        );
        assert_eq!(
            snapshot.get("HOME_ONLY_KEY").map(String::as_str),
            Some("home-only"),
            "the Harness home backs keys the project does not set"
        );

        std::fs::write(project.join(".env"), "MARKER_ACCOUNT_API_KEY=edited\n").unwrap();
        assert_eq!(
            snapshot.get("MARKER_ACCOUNT_API_KEY").map(String::as_str),
            Some("last"),
            "a launch snapshot does not follow later dotenv edits"
        );
    }

    #[test]
    fn the_service_inherits_only_its_stores_and_its_tools() {
        let environment = gateway_environment(Path::new("/home/dev/.local/share/zeron/deepseek"));
        assert_eq!(
            environment
                .get(&OsString::from("DSH_HOME"))
                .map(|value| value.to_string_lossy().into_owned()),
            Some("/home/dev/.local/share/zeron/deepseek".to_owned())
        );
        assert!(!environment.contains_key(&OsString::from("DEEPSEEK_API_KEY")));
        assert!(!environment.contains_key(&OsString::from("OPENAI_API_KEY")));
    }
}
