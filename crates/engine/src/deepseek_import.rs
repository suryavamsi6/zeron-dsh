//! DeepSeek v3 chat-export import (`ImportDeepSeekChats`).
//!
//! A `.zip` export on this device becomes ordinary chat rows + transcript
//! docs here, through the same write paths every other chat birth uses:
//! the pinned `chat-export.mjs` parser (`zeron_harness`) normalizes and
//! branch-selects; [`SessionDoc::init`] + [`TranscriptStore`] land each doc
//! in the born-chat2 shape the local→synced import uses; `import_chat_row`
//! publishes the row last — the fork order, so a crash mid-import can never
//! leave a discoverable empty chat.
//!
//! Deterministic identity makes a re-import a no-op: the row id derives
//! from the export's own conversation id, so the same export finds the
//! rows it already wrote instead of duplicating them.
//!
//! The `imported` marker the rows carry is also the read-only flag — an
//! imported chat has no harness session and must never start a turn.
//! [`READ_ONLY`] is the one refusal every send path shares, so the person
//! hears the reason verbatim wherever they tried.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionDoc, SessionMessageEntry};
use zeron_harness::deepseek::chat_export::{ExportedConversation, ExportedRole, ExportedTurn};
use zeron_proto::{Chat, ImportedChat};
use zeron_rpc::{RpcError, parse_params};

use crate::EngineError;
use crate::workspace_host::WorkspaceHost;

/// The refusal a send into an imported chat gets, verbatim at every
/// adoption point (command, queued row, drain, dispatch) so the composer,
/// the command plane and a remote peer's write all say the same thing.
pub(crate) const READ_ONLY: &str =
    "This chat was imported from DeepSeek and is read-only.";

/// What [`ImportedChat::source`] records — the pinned parser handles
/// exactly the DeepSeek v3 export shape.
const SOURCE: &str = "deepseek-v3";

/// Sidebar preview length: about a line of the last turn's text.
const PREVIEW_CHARS: usize = 200;

pub(crate) fn handles(method: &str) -> bool {
    method == zeron_rpc::methods::IMPORT_DEEPSEEK_CHATS
}

/// `{path}` → `{imported, alreadyPresent, chats}`. `EngineRpc`'s dispatch
/// reaches here via [`handles`], off the async dispatcher (node + SQLite +
/// fs all block), after the `local_only` transport policy — the path names
/// a file on this machine, read only here.
pub(crate) fn call(
    store: Arc<zeron_sync::DocsStore>,
    workspace: WorkspaceHost,
    device_id: String,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Params {
        path: String,
    }
    let params: Params = parse_params(params)?;
    let outcome = ChatImporter::new(store, workspace, device_id)
        .import(Path::new(&params.path))
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    serde_json::to_value(&outcome).map_err(|error| RpcError::Failed(error.to_string()))
}

/// Where an imported transcript lands before its row is published. The
/// production impl is [`zeron_sync::DocsStore`]'s born-chat2 write; tests
/// substitute a failing store to pin the transcript-before-row ordering.
pub trait TranscriptStore: Send + Sync {
    fn save(&self, chat_id: &str, bytes: &[u8]) -> Result<(), String>;
}

impl TranscriptStore for zeron_sync::DocsStore {
    fn save(&self, chat_id: &str, bytes: &[u8]) -> Result<(), String> {
        self.save_snapshot_with_cursor(chat_id, bytes, 0, crate::chat2_host::CHAT2_DOC_EPOCH)
            .map_err(|error| error.to_string())
    }
}

/// What an import did, in the export's own order: `chats` lists every
/// conversation's Zeron chat id whether it imported or was already there —
/// the list is the same across re-imports (determinism is observable).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub imported: usize,
    pub already_present: usize,
    pub chats: Vec<String>,
}

/// The importer: transcript bytes through [`TranscriptStore`], rows through
/// [`WorkspaceHost`], entries stamped with the importing device.
pub struct ChatImporter {
    store: Arc<dyn TranscriptStore>,
    workspace: WorkspaceHost,
    device_id: String,
}

impl ChatImporter {
    pub fn new(
        store: Arc<dyn TranscriptStore>,
        workspace: WorkspaceHost,
        device_id: String,
    ) -> Self {
        Self {
            store,
            workspace,
            device_id,
        }
    }

    /// Read `zip` through the pinned v3 parser and land its conversations as
    /// chat rows + transcript docs, in the export's own order. Rows an
    /// earlier import already wrote count `already_present` and are left
    /// alone — the deterministic id IS the idempotence, so neither doc nor
    /// row is touched. The whole call runs under one lock: two concurrent
    /// imports must not both find a row missing and mint it twice.
    pub fn import(&self, zip: &Path) -> Result<Outcome, EngineError> {
        let _guard = import_lock();
        let conversations = zeron_harness::deepseek::chat_export::read_chat_export(zip)
            .map_err(EngineError::Other)?;
        // One stamp for the whole call: every row the import writes says
        // the same "when".
        let imported_at = Utc::now();
        let mut outcome = Outcome {
            imported: 0,
            already_present: 0,
            chats: Vec::new(),
        };
        for conversation in &conversations {
            let chat_id = format!("deepseek-{}", conversation.id);
            outcome.chats.push(chat_id.clone());
            if self.workspace.chat(&chat_id)?.is_some() {
                outcome.already_present += 1;
                continue;
            }
            self.import_conversation(conversation, &chat_id, imported_at)?;
            outcome.imported += 1;
        }
        Ok(outcome)
    }

    /// One conversation: the transcript is durable before the discoverable
    /// row exists, so a store error publishes nothing. The row's fields come
    /// from the export or the neutral defaults a chat that never ran needs,
    /// and its doc is born chat2 (cursor 0, epoch [`CHAT2_DOC_EPOCH`]) so
    /// `DocHost::open` never routes it down the legacy s2 adopt path.
    ///
    /// [`CHAT2_DOC_EPOCH`]: crate::chat2_host::CHAT2_DOC_EPOCH
    fn import_conversation(
        &self,
        conversation: &ExportedConversation,
        chat_id: &str,
        imported_at: DateTime<Utc>,
    ) -> Result<(), EngineError> {
        // The export carries no per-turn stamp; the conversation's own times
        // are what the transcript falls back on, spread evenly across them.
        let start = conversation
            .inserted_at
            .as_deref()
            .and_then(rfc3339)
            .unwrap_or(imported_at);
        let end = conversation
            .updated_at
            .as_deref()
            .and_then(rfc3339)
            .unwrap_or(start);
        let (start_ms, end_ms) = (start.timestamp_millis(), end.timestamp_millis());

        // A turn becomes an entry when it carries anything at all; the time
        // spread counts only the entries written, so a skipped turn leaves
        // no hole in either index or time.
        let turns: Vec<&ExportedTurn> = conversation
            .turns
            .iter()
            .filter(|turn| {
                !(turn.text.is_empty() && turn.thinking.is_empty() && turn.links.is_empty())
            })
            .collect();
        let doc = SessionDoc::init(chat_id)?;
        let mut last_text = None;
        for (index, turn) in turns.iter().enumerate() {
            let created_at = if turns.len() > 1 {
                start_ms + (end_ms - start_ms) * index as i64 / (turns.len() - 1) as i64
            } else {
                start_ms
            };
            if !turn.text.is_empty() {
                last_text = Some(turn.text.clone());
            }
            doc.push_message(&SessionMessageEntry {
                id: format!("{chat_id}:{index}"),
                role: match turn.role {
                    ExportedRole::User => MessageRole::User,
                    ExportedRole::Assistant => MessageRole::Assistant,
                },
                parts: parts(turn),
                created_at,
                device_id: self.device_id.clone(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
                duration_ms: None,
            })?;
        }
        self.store
            .save(chat_id, &doc.export_snapshot()?)
            .map_err(EngineError::Other)?;
        self.workspace.import_chat_row(&Chat {
            id: chat_id.to_string(),
            device_id: self.device_id.clone(),
            title: conversation.title.clone(),
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: last_text.as_deref().map(preview),
            last_message_at: Some(end),
            created_at: start,
            harness_session_id: None,
            harness_session_cwd: None,
            harness_session_harness: None,
            space_id: None,
            last_seen_at: None,
            room_gen: Some(crate::chat2_host::CHAT2_DOC_EPOCH),
            parent_chat_id: None,
            child_kind: None,
            sleep: None,
            imported: Some(ImportedChat {
                source: SOURCE.into(),
                source_id: conversation.id.clone(),
                imported_at,
            }),
        })?;
        Ok(())
    }
}

/// Two concurrent imports over the same export would both see no row and
/// push the same turns twice into one doc; the whole `import` call is one
/// critical section.
fn import_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One turn's parts, in transcript order: its text, then its thinking,
/// then its links as one markdown list. The parser already separated all
/// three; empty ones are left out.
fn parts(turn: &ExportedTurn) -> Vec<MessagePart> {
    let mut parts = Vec::new();
    if !turn.text.is_empty() {
        parts.push(MessagePart::Text {
            id: "t0".into(),
            text: turn.text.clone(),
        });
    }
    if !turn.thinking.is_empty() {
        parts.push(MessagePart::Reasoning {
            id: "r0".into(),
            text: turn.thinking.clone(),
        });
    }
    if !turn.links.is_empty() {
        let text = turn
            .links
            .iter()
            .map(|link| match &link.title {
                Some(title) => format!("- [{title}]({})", link.url),
                None => format!("- {}", link.url),
            })
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(MessagePart::Text {
            id: "l0".into(),
            text,
        });
    }
    parts
}

/// An RFC3339 export timestamp; an unparseable value is left out rather
/// than failing the whole import.
fn rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .ok()
}

/// The last turn's text as a sidebar line, cut at `PREVIEW_CHARS`.
fn preview(text: &str) -> String {
    text.chars().take(PREVIEW_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeron_rpc::{RpcReply, RpcService, methods};

    fn fixture() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../harness/tests/fixtures/deepseek-export-v3.zip")
    }

    /// One engine rig: its own temp data dir, Mock harness, no edge.
    async fn rig() -> (tempfile::TempDir, crate::EngineCore, Arc<crate::rpc::EngineRpc>) {
        let temp = tempfile::tempdir().unwrap();
        let core = crate::EngineCore::assemble_with_profile(
            crate::EngineProfile::local(temp.path()).unwrap(),
            Arc::new(crate::HarnessRegistry::new()),
            crate::HarnessId::Mock,
            None,
        )
        .unwrap();
        let rpc = core.rpc_service();
        (temp, core, rpc)
    }

    fn entry_count(core: &crate::EngineCore, chat_id: &str) -> usize {
        core.doc_host
            .open(chat_id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn a_v3_export_imports_then_reimports_as_already_present() {
        let (_temp, core, rpc) = rig().await;
        assert!(handles(methods::IMPORT_DEEPSEEK_CHATS));
        let params = serde_json::json!({ "path": fixture() });
        let RpcReply::Value(first) = rpc
            .handle(methods::IMPORT_DEEPSEEK_CHATS, params.clone())
            .await
            .unwrap()
        else {
            panic!("the import answers with a value");
        };
        assert_eq!(first["imported"], serde_json::json!(2));
        assert_eq!(first["alreadyPresent"], serde_json::json!(0));
        let chats = first["chats"].clone();
        assert_eq!(chats, serde_json::json!(["deepseek-s2", "deepseek-s1"]));

        assert_eq!(entry_count(&core, "deepseek-s2"), 2);
        assert_eq!(entry_count(&core, "deepseek-s1"), 4);
        let row = core.workspace.chat("deepseek-s1").unwrap().unwrap();
        assert_eq!(row.title.as_deref(), Some("Two turns"));
        let marker = row.imported.as_ref().expect("the import marks the row");
        assert_eq!(marker.source, "deepseek-v3");
        assert_eq!(marker.source_id, "s1");

        let RpcReply::Value(second) = rpc
            .handle(methods::IMPORT_DEEPSEEK_CHATS, params)
            .await
            .unwrap()
        else {
            panic!("the re-import answers with a value");
        };
        assert_eq!(second["imported"], serde_json::json!(0));
        assert_eq!(second["alreadyPresent"], serde_json::json!(2));
        assert_eq!(second["chats"], chats, "same ids, same order");
        assert_eq!(entry_count(&core, "deepseek-s2"), 2, "the reimport rewrote nothing");
        assert_eq!(entry_count(&core, "deepseek-s1"), 4, "the reimport rewrote nothing");
        core.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn an_imported_chat_refuses_a_send() {
        let (_temp, core, rpc) = rig().await;
        rpc.handle(
            methods::IMPORT_DEEPSEEK_CHATS,
            serde_json::json!({ "path": fixture() }),
        )
        .await
        .unwrap();
        // The composer path and the durable-command path refuse alike.
        for (method, params) in [
            (
                methods::QUEUE_COMMAND,
                serde_json::json!({
                    "chatId": "deepseek-s1",
                    "command": {
                        "kind": "run",
                        "messageId": "m1",
                        "request": {
                            "prompt": "again",
                            "cwd": "/tmp",
                            "sandbox": "workspace-write"
                        }
                    }
                }),
            ),
            (
                methods::QUEUE_MESSAGE,
                serde_json::json!({ "chatId": "deepseek-s1", "text": "again" }),
            ),
        ] {
            let Err(RpcError::Failed(message)) = rpc.handle(method, params).await else {
                panic!("{method} must not send into an imported chat");
            };
            assert_eq!(message, READ_ONLY, "{method}");
        }
        core.sessions.shutdown().await;
    }

    #[tokio::test]
    async fn a_bad_path_is_a_displayable_error() {
        let (_temp, core, rpc) = rig().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-an-export.zip");
        std::fs::write(&path, b"this is not a zip").unwrap();
        let Err(RpcError::Failed(message)) = rpc
            .handle(
                methods::IMPORT_DEEPSEEK_CHATS,
                serde_json::json!({ "path": path }),
            )
            .await
        else {
            panic!("a non-export must not import");
        };
        assert!(!message.is_empty(), "the reason must be displayable");
        assert!(
            !message.to_lowercase().contains("panic"),
            "no panic text in: {message}"
        );
        core.sessions.shutdown().await;
    }
}
