//! SUR-119: an imported chat is read-only at the engine, not just in a UI.
//!
//! Presence of `Chat.imported` IS the marker (see `zeron_proto::ImportedChat`:
//! an imported conversation has no local provider session and must never start
//! a turn here). The row is otherwise ordinary, so every send path has to ask.

use std::sync::Arc;

use zeron_engine::{EngineCore, EngineProfile, HarnessId, default_registry};
use zeron_proto::ImportedChat;

fn assemble(dir: &std::path::Path) -> EngineCore {
    EngineCore::assemble_with_profile(
        EngineProfile::local(dir).expect("local profile"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .expect("assemble profile")
}

/// A chat row carrying the import marker, written the way an import writes it.
fn mark_imported(core: &EngineCore, chat_id: &str) {
    let mut row = core
        .workspace
        .chat(chat_id)
        .expect("read chat")
        .expect("chat row");
    row.imported = Some(ImportedChat {
        source: "deepseek-v3".into(),
        source_id: chat_id.into(),
        imported_at: chrono::Utc::now(),
    });
    core.workspace.import_chat_row(&row).expect("import marker");
}

#[tokio::test]
async fn a_message_to_an_imported_chat_is_refused() {
    use zeron_rpc::RpcService as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());
    core.workspace
        .create_chat("imported", None, Some(&core.device_id), None, None)
        .expect("chat row");
    mark_imported(&core, "imported");

    let rpc = core.rpc_service();
    let refused = rpc
        .handle(
            zeron_rpc::methods::QUEUE_MESSAGE,
            serde_json::json!({ "chatId": "imported", "text": "hello" }),
        )
        .await;
    let error = match refused {
        Err(error) => error.to_string(),
        Ok(_) => panic!("a send to an imported chat was accepted"),
    };
    assert!(
        error.contains("read-only"),
        "the refusal must say why: {error}"
    );

    // Refused means nothing was left behind: no queue row for a later drain.
    let handle = core.doc_host.open("imported").expect("open doc");
    assert!(
        handle.watch_queue().borrow().is_empty(),
        "the refused send queued a row anyway"
    );

    core.shutdown().await;
}

#[tokio::test]
async fn a_message_to_an_ordinary_chat_is_accepted() {
    use zeron_rpc::RpcService as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let core = assemble(dir.path());
    core.workspace
        .create_chat("ordinary", None, Some(&core.device_id), None, None)
        .expect("chat row");

    let rpc = core.rpc_service();
    rpc.handle(
        zeron_rpc::methods::QUEUE_MESSAGE,
        serde_json::json!({ "chatId": "ordinary", "text": "hello" }),
    )
    .await
    .expect("an ordinary chat accepts a send");

    core.shutdown().await;
}
