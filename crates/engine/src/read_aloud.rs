//! `ReadAloud`: an answer's text as speech — DeepSeek's TTS API, returning
//! 24 kHz 16-bit mono PCM the caller plays. The key it spends resolves the
//! way a run's does; it never leaves the engine, so the method is this
//! device's own IPC only (`zeron_rpc::local_only`).

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use zeron_proto::VaultProvider;
use zeron_rpc::RpcError;

use crate::credentials::{CredentialBroker, GrantOutcome};
use crate::http_error::describe_http_error;

/// DeepSeek's API origin; the call posts to `{base}/v1/tools/tts`.
const API_BASE: &str = "https://api.deepseek.com";
/// The reply's fixed shape: raw s16le mono PCM at this rate.
const SAMPLE_RATE: u32 = 24_000;
/// DeepSeek bills at most this many characters a call; a longer answer is
/// read through its first that many, clipped at a char boundary.
const MAX_BILLED_CHARS: usize = 10_000;
/// Synthesis streams; a long answer takes a while, a lost socket this long.
const TTS_TIMEOUT: Duration = Duration::from_secs(120);
/// The voices the API takes (`voice_id`); `stella` answers a call that
/// names none.
const VOICES: [&str; 2] = ["stella", "tide"];
/// Where to put a key when none resolves.
const NO_KEY: &str = "Add a DeepSeek API key in Settings to read answers aloud";

/// Why a read-aloud failed; the message is written for the person.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReadAloudError(String);

/// One `ReadAloud` call: `{text, voice?}` → `{sampleRate, channels, pcm}`.
pub async fn call(
    credentials: Option<&CredentialBroker>,
    deepseek_home: &Path,
    params: &Value,
) -> Result<Value, RpcError> {
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::BadParams("ReadAloud takes a text string".into()))?;
    let voice = match params.get("voice") {
        None | Some(Value::Null) => None,
        Some(voice) => Some(
            voice
                .as_str()
                .ok_or_else(|| RpcError::BadParams("ReadAloud's voice is a string".into()))?,
        ),
    };
    let key = api_key(credentials, deepseek_home)
        .await
        .ok_or_else(|| RpcError::Failed(NO_KEY.into()))?;
    let pcm = synthesize(&client(), API_BASE, &key, text, voice)
        .await
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    Ok(json!({
        "sampleRate": SAMPLE_RATE,
        "channels": 1,
        "pcm": base64::engine::general_purpose::STANDARD.encode(pcm),
    }))
}

/// `text` as PCM audio, or why not. `voice` is one of [`VOICES`], checked
/// before any request goes out.
pub async fn synthesize(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    text: &str,
    voice: Option<&str>,
) -> Result<Vec<u8>, ReadAloudError> {
    let voice = match voice.unwrap_or(VOICES[0]) {
        voice if VOICES.contains(&voice) => voice,
        other => {
            return Err(ReadAloudError(format!(
                "Unknown voice {other:?} — pick \"stella\" or \"tide\""
            )));
        }
    };
    if text.trim().is_empty() {
        return Err(ReadAloudError("There's nothing to read aloud".into()));
    }
    // Clip at a char boundary: the API bills 10,000 characters a call.
    let text = &text[..text
        .char_indices()
        .nth(MAX_BILLED_CHARS)
        .map(|(at, _)| at)
        .unwrap_or(text.len())];
    let response = client
        .post(format!("{}/v1/tools/tts", base.trim_end_matches('/')))
        .bearer_auth(key)
        .json(&json!({
            "text": text,
            "voice_id": voice,
            "format": "pcm",
            "filter_markdown": true,
        }))
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        let detail = error_detail(&response.bytes().await.unwrap_or_default());
        return Err(match status.as_u16() {
            400 if detail.to_ascii_lowercase().contains("content exists risk") => ReadAloudError(
                "DeepSeek flagged that text for content review and won't read it aloud".into(),
            ),
            400 => ReadAloudError("DeepSeek found nothing it can read aloud in that text".into()),
            429 => ReadAloudError(
                "DeepSeek's speech service has too many requests in flight; try again in a moment"
                    .into(),
            ),
            503 => {
                ReadAloudError("DeepSeek's speech service is busy; try again in a moment".into())
            }
            504 => ReadAloudError("DeepSeek's speech service took too long; try again".into()),
            _ => ReadAloudError(format!(
                "Read-aloud failed (HTTP {}): {detail}",
                status.as_u16()
            )),
        });
    }
    // A body that ends early is a failure, not a short answer.
    let expected = response.content_length();
    let pcm = response.bytes().await.map_err(failed)?;
    if expected.is_some_and(|expected| pcm.len() as u64 != expected) {
        return Err(ReadAloudError(
            "DeepSeek ended the audio before it was done; try again".into(),
        ));
    }
    if pcm.is_empty() {
        return Err(ReadAloudError(
            "DeepSeek returned no audio for that text".into(),
        ));
    }
    Ok(pcm.to_vec())
}

/// The DeepSeek key a run on this device would get: the vault's grant where
/// this engine is the Cloud runner, else the stored key runs fall back to,
/// else the engine's own environment.
async fn api_key(broker: Option<&CredentialBroker>, home: &Path) -> Option<String> {
    if let Some(broker) = broker
        && let GrantOutcome::Granted(grant) = broker.grant(VaultProvider::DeepseekKey).await
    {
        return Some(grant.access_token);
    }
    std::fs::read_to_string(home.join("api-key"))
        .ok()
        .filter(|key| valid_key(key))
        .or_else(|| {
            std::env::var("DEEPSEEK_API_KEY")
                .ok()
                .filter(|key| valid_key(key))
        })
}

/// The same rule the account store holds a key to (`agent_accounts`).
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 512 && !key.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The API's own message out of an error body (`{"error":{"message":…}}`),
/// else a clipped piece of whatever it sent.
fn error_detail(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| {
            value["error"]["message"]
                .as_str()
                .or_else(|| value["message"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| text.chars().take(200).collect())
}

fn failed(error: reqwest::Error) -> ReadAloudError {
    ReadAloudError(format!(
        "Read-aloud couldn't reach DeepSeek: {}",
        describe_http_error(error)
    ))
}

/// TTS bodies stream, so this rides a longer timeout than the runner's.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TTS_TIMEOUT)
        .user_agent(concat!("zeron/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// What a fake TTS heard: `(head, body)` per request.
    type Heard = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    /// A TTS endpoint on a local port: remembers each request's head and
    /// body and answers it `status` with `reply`.
    async fn fake_tts(status: &'static str, reply: &'static [u8]) -> (String, Heard) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let heard: Heard = Arc::default();
        let log = heard.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let head_end = loop {
                        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                        let mut chunk = vec![0; 64 * 1024];
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        let mut chunk = vec![0; 64 * 1024];
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    log.lock()
                        .unwrap()
                        .push((head, buf[head_end..head_end + len].to_vec()));
                    let _ = socket
                        .write_all(
                            format!(
                                "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n",
                                reply.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = socket.write_all(reply).await;
                });
            }
        });
        (url, heard)
    }

    #[tokio::test]
    async fn sends_the_bearer_header_and_the_json_body() {
        let (url, heard) = fake_tts("200 OK", b"\x01\x00\x02\x00").await;
        let pcm = synthesize(
            &reqwest::Client::new(),
            &url,
            "sk-test",
            "hello",
            Some("tide"),
        )
        .await
        .unwrap();
        assert_eq!(pcm, b"\x01\x00\x02\x00");
        let (head, body) = heard.lock().unwrap().remove(0);
        assert!(head.starts_with("POST /v1/tools/tts "), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer sk-test"),
            "{head}"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"text": "hello", "voice_id": "tide", "format": "pcm", "filter_markdown": true})
        );
    }

    #[tokio::test]
    async fn a_content_review_400_reads_as_a_refusal() {
        let (url, _) = fake_tts(
            "400 Bad Request",
            br#"{"error":{"message":"Content Exists Risk"}}"#,
        )
        .await;
        let error = synthesize(&reqwest::Client::new(), &url, "sk-test", "text", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("content review"), "{error}");
    }

    #[tokio::test]
    async fn an_unknown_voice_fails_before_any_request() {
        let (url, heard) = fake_tts("200 OK", b"\x01\x00").await;
        let error = synthesize(
            &reqwest::Client::new(),
            &url,
            "sk-test",
            "text",
            Some("bob"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("bob"), "{error}");
        assert!(heard.lock().unwrap().is_empty());
    }
}
