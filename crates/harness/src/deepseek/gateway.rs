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

    /// Call one gateway method with its argument object.
    pub async fn call(&self, method: &str, args: Value) -> Result<Value, GatewayError> {
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
        let body = request_body(method, args);
        let response = self
            .http
            .post(format!("{}/api/{method}", session.base))
            .header(reqwest::header::COOKIE, &session.cookie)
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

    /// Stop the service if it is running; the next call starts a fresh one.
    pub async fn shutdown(&self) {
        let mut guard = self.session.lock().await;
        if let Some(mut session) = guard.take() {
            let _ = session.child.kill().await;
        }
    }

    fn bin(&self) -> PathBuf {
        self.sdk_root
            .join("node_modules/@deepseek-ai/dsh/lib/bin.js")
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
        let mut command = Command::new(&node);
        command
            .arg(&bin)
            .arg("--profile")
            .arg("web")
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

/// What a service process inherits: enough to find its stores and its own
/// tools, nothing of the app's environment that could change its behaviour.
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
