//! SUR-119: importing a DeepSeek v3 export as read-only chats.
//!
//! The parser seam is pinned in `chat_export_import.rs` of the harness crate;
//! this file pins the engine seam: one device-local RPC, a transcript written
//! before its row, a stable source identity for re-imports, and the read-only
//! marker persisted with the row.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zeron_doc::{MessagePart, MessageRole, SessionMessageEntry};
use zeron_engine::deepseek_import::{ChatImporter, TranscriptStore};
use zeron_engine::{EngineCore, EngineProfile, HarnessId, default_registry};
use zeron_rpc::RpcService as _;

fn assemble(dir: &Path) -> EngineCore {
    EngineCore::assemble_with_profile(
        EngineProfile::local(dir).expect("local profile"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .expect("assemble profile")
}

/// The v3 fixture belongs to the parser it exercises (harness tests).
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("harness")
        .join("tests")
        .join("fixtures")
        .join("deepseek-export-v3.zip")
}

/// One conversation with no messages, one whose only message the exporter
/// marked `CONTENT_FILTER`, and one with no title.
fn empty_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("deepseek-export-v3-empty.zip")
}

async fn import(core: &EngineCore, path: &Path) -> Result<serde_json::Value, String> {
    let rpc = core.rpc_service();
    match rpc
        .handle(
            zeron_rpc::methods::IMPORT_DEEPSEEK_CHATS,
            serde_json::json!({ "path": path }),
        )
        .await
    {
        Ok(zeron_rpc::RpcReply::Value(value)) => Ok(value),
        Ok(_) => Err("the import must answer with a value".into()),
        Err(error) => Err(error.to_string()),
    }
}

fn texts(entry: &SessionMessageEntry) -> Vec<&str> {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn reasoning(entry: &SessionMessageEntry) -> Vec<&str> {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn entry(core: &EngineCore, chat_id: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(chat_id)
        .expect("open imported doc")
        .doc_arc()
        .read_entries()
        .expect("read imported transcript")
}

#[tokio::test]
async fn an_export_imports_as_read_only_chats_with_ordered_transcripts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());

    let summary = import(&core, &fixture()).await.expect("import");
    assert_eq!(summary["imported"], 2, "{summary}");
    assert_eq!(summary["alreadyPresent"], 0, "{summary}");
    // The parser's own order, and one id per conversation: deterministic, so a
    // re-import can find what it wrote.
    assert_eq!(
        summary["chats"],
        serde_json::json!(["deepseek-s2", "deepseek-s1"]),
        "{summary}"
    );

    // The row is present, clickable, and says where it came from — that is the
    // read-only marker, so it is persisted, not a UI state.
    let two = core
        .workspace
        .chat("deepseek-s1")
        .expect("read imported row")
        .expect("s1 imported");
    assert_eq!(two.title.as_deref(), Some("Two turns"));
    assert_eq!(two.room_gen, Some(2), "imported docs are chat2-born");
    assert_eq!(
        two.space_id, None,
        "no project is invented; the UI places it"
    );
    assert_eq!(two.config, None, "an imported chat has no harness to run");
    assert_eq!(
        two.created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "2026-11-01T00:00:00Z",
        "the export's own start time is preserved"
    );
    assert_eq!(
        two.last_message_at
            .expect("last message time")
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "2026-11-02T00:00:00Z",
        "the export's own last-message time is preserved"
    );
    let marker = two.imported.as_ref().expect("the import marks the row");
    assert_eq!(marker.source, "deepseek-v3");
    assert_eq!(marker.source_id, "s1", "the exporter's id is the identity");

    // The doc is stored in the born-chat2 shape: cursor 0 and epoch 2. With
    // the row's `roomGen: 2` that is exactly what keeps `DocHost::open` off
    // the legacy s2 adopt path — an epoch-0 doc under an edge profile is
    // discarded there, which would lose this transcript.
    let (stored, cursor, epoch) = core
        .doc_host
        .store()
        .load_snapshot_with_cursor("deepseek-s1")
        .expect("read the stored doc")
        .expect("the transcript is stored");
    assert_eq!((cursor, epoch), (0, 2), "born-chat2: cursor 0, epoch 2");
    assert!(!stored.is_empty(), "the stored doc carries the transcript");

    // Role, order, text, thinking and links — the parser already selected the
    // branch, so the transcript is exactly its turns in its order.
    let turns = entry(&core, &two.id);
    let roles: Vec<MessageRole> = turns.iter().map(|turn| turn.role).collect();
    assert_eq!(
        roles,
        vec![
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant
        ]
    );
    assert_eq!(texts(&turns[0]), vec!["first question"]);
    assert_eq!(texts(&turns[1])[0], "first answer");
    assert_eq!(reasoning(&turns[1]), vec!["reasoning"]);
    assert!(
        texts(&turns[1])
            .iter()
            .any(|text| text.contains("https://a.example") && text.contains("https://b.example")),
        "the assistant turn's links are in the transcript: {:?}",
        texts(&turns[1])
    );
    assert_eq!(texts(&turns[2]), vec!["second question"]);
    assert_eq!(texts(&turns[3])[0], "second answer");
    assert!(
        texts(&turns[3])
            .iter()
            .any(|text| text.contains("https://c.example")),
        "the last turn's links are in the transcript: {:?}",
        texts(&turns[3])
    );
    let times: Vec<i64> = turns.iter().map(|turn| turn.created_at).collect();
    assert!(
        times.windows(2).all(|pair| pair[0] <= pair[1]),
        "entry times follow the transcript order: {times:?}"
    );
    assert_eq!(
        times.first().copied(),
        Some(two.created_at.timestamp_millis()),
        "the first entry starts when the conversation did"
    );
    assert_eq!(
        times.last().copied(),
        Some(two.last_message_at.expect("last").timestamp_millis()),
        "the last entry ends when the conversation did"
    );

    // The other conversation in the same export imports independently.
    let simple = core
        .workspace
        .chat("deepseek-s2")
        .expect("read imported row")
        .expect("s2 imported");
    assert_eq!(simple.title.as_deref(), Some("Simple"));
    assert_eq!(
        simple.imported.as_ref().expect("marker").source_id,
        "s2",
        "one export, one identity per conversation"
    );
    assert_eq!(
        simple.imported.as_ref().expect("marker").imported_at,
        marker.imported_at,
        "one call stamps every row it writes with one import time"
    );
    let simple_turns = entry(&core, &simple.id);
    assert_eq!(simple_turns.len(), 2);
    assert_eq!(texts(&simple_turns[0]), vec!["hello"]);
    assert_eq!(texts(&simple_turns[1]), vec!["hi"]);

    core.shutdown().await;
}

#[tokio::test]
async fn a_zero_turn_conversation_writes_no_chat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());

    let summary = import(&core, &empty_fixture()).await.expect("import");
    // The parser emits `turns: []` for both; there is nothing to open, so no
    // row, no count and no id in the answer.
    assert_eq!(summary["imported"], 1, "{summary}");
    assert_eq!(summary["alreadyPresent"], 0, "{summary}");
    assert_eq!(
        summary["chats"],
        serde_json::json!(["deepseek-u1"]),
        "{summary}"
    );
    for silent in ["e1", "e2"] {
        assert!(
            core.workspace
                .chat(&format!("deepseek-{silent}"))
                .expect("read row")
                .is_none(),
            "{silent} has no turns and must not become a chat"
        );
    }

    // The conversation that does have a turn imports, and a missing export
    // title stays missing rather than being invented.
    let untitled = core
        .workspace
        .chat("deepseek-u1")
        .expect("read row")
        .expect("u1 imported");
    assert_eq!(untitled.title, None, "a null title stays null");
    assert_eq!(entry(&core, "deepseek-u1").len(), 1);

    core.shutdown().await;
}

#[tokio::test]
async fn reopening_the_profile_keeps_the_imports_read_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());
    import(&core, &fixture()).await.expect("import");
    let chat_id = core
        .workspace
        .chat("deepseek-s1")
        .expect("read row")
        .expect("s1 imported")
        .id;
    core.shutdown().await;
    drop(core);

    // A fresh engine over the same data dir is what the next app launch does.
    let reopened = assemble(dir.path());
    let row = reopened
        .workspace
        .chat(&chat_id)
        .expect("read chat")
        .expect("the imported row survived a restart");
    assert!(
        row.imported.is_some(),
        "read-only is persisted with the row, not held in memory"
    );
    assert_eq!(row.imported.expect("marker").source_id, "s1");

    let turns = entry(&reopened, &chat_id);
    assert_eq!(turns.len(), 4, "the transcript survived a restart");
    assert_eq!(texts(&turns[0]), vec!["first question"]);
    assert_eq!(texts(&turns[3])[0], "second answer");
    reopened.shutdown().await;
}

#[tokio::test]
async fn reimporting_the_same_export_reports_it_already_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());

    import(&core, &fixture()).await.expect("first import");
    let again = import(&core, &fixture()).await.expect("second import");
    assert_eq!(again["imported"], 0, "{again}");
    assert_eq!(again["alreadyPresent"], 2, "{again}");
    assert_eq!(
        again["chats"],
        serde_json::json!(["deepseek-s2", "deepseek-s1"]),
        "the same ids come back, so a retry is the same chats"
    );
    assert_eq!(
        entry(&core, "deepseek-s1").len(),
        4,
        "the re-import rewrote nothing"
    );

    core.shutdown().await;
}

/// A transcript write that fails must leave no row behind: the row is what
/// makes a chat clickable, and an empty clickable chat is the failure this
/// ordering exists to prevent.
struct FailingStore;

impl TranscriptStore for FailingStore {
    fn save(&self, _chat_id: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("disk full (injected)".into())
    }
}

#[tokio::test]
async fn a_failed_transcript_write_publishes_no_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());

    let importer = ChatImporter::new(
        Arc::new(FailingStore),
        core.workspace.clone(),
        core.device_id.clone(),
    );
    let error = importer.import(&fixture()).expect_err("the doc write fails");
    assert!(
        error.to_string().contains("disk full"),
        "the failure is the write's own: {error}"
    );
    for chat_id in ["deepseek-s1", "deepseek-s2"] {
        assert!(
            core.workspace.chat(chat_id).expect("read row").is_none(),
            "{chat_id} was published without a transcript"
        );
    }

    core.shutdown().await;
}

#[tokio::test]
async fn a_non_export_file_is_a_displayable_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("not-an-export.zip");
    std::fs::write(&path, b"this is not a zip").expect("write");
    let core = assemble(dir.path());

    let error = import(&core, &path)
        .await
        .expect_err("a non-export is refused");
    assert_eq!(
        error, "chat-export: not a ZIP archive",
        "the parser's own message reaches the person, not a Node stack"
    );
    assert!(
        !error.contains("    at "),
        "no stack frames in the displayable error: {error}"
    );
    assert!(
        core.workspace
            .chat("deepseek-s1")
            .expect("read row")
            .is_none(),
        "a refused import wrote nothing"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn another_device_cannot_import_into_this_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());
    let rpc = core.rpc_service();

    for caller in [
        zeron_rpc::Caller::Peer { device: None },
        zeron_rpc::Caller::Agent {
            chat: Some("chat-1".into()),
        },
    ] {
        let refused = zeron_rpc::with_caller(caller.clone(), async {
            rpc.handle(
                zeron_rpc::methods::IMPORT_DEEPSEEK_CHATS,
                serde_json::json!({ "path": fixture() }),
            )
            .await
        })
        .await;
        assert!(refused.is_err(), "an import was served to {caller:?}");
    }
    assert!(
        core.workspace
            .chat("deepseek-s1")
            .expect("read row")
            .is_none(),
        "a refused import wrote nothing"
    );

    core.shutdown().await;
}
