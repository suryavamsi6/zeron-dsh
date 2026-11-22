//! SUR-119: the engine imports a DeepSeek v3 export through the pinned
//! `chat-export.mjs` parser. This pins the Rust side of that seam: the parser
//! output arrives normalized, in its own order, with the branch already
//! selected — the engine never re-implements either.

use std::path::PathBuf;

use zeron_harness::deepseek::chat_export::read_chat_export;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("deepseek-export-v3.zip")
}

#[test]
fn the_v3_export_reads_into_normalized_conversations() {
    let conversations = read_chat_export(&fixture()).expect("the fixture parses");
    assert_eq!(conversations.len(), 2, "both conversations import");

    // The parser orders newest-updated first; the engine keeps that order.
    let simple = &conversations[0];
    assert_eq!(simple.id, "s2");
    assert_eq!(simple.title.as_deref(), Some("Simple"));
    assert_eq!(simple.turns.len(), 2);
    assert_eq!(simple.turns[0].role, "user");
    assert_eq!(simple.turns[0].text, "hello");
    assert_eq!(simple.turns[1].role, "assistant");
    assert_eq!(simple.turns[1].text, "hi");

    let two = &conversations[1];
    assert_eq!(two.id, "s1");
    assert_eq!(two.title.as_deref(), Some("Two turns"));
    assert_eq!(two.inserted_at.as_deref(), Some("2026-11-01T00:00:00Z"));
    assert_eq!(two.updated_at.as_deref(), Some("2026-11-02T00:00:00Z"));
    // The parser selected `current_message_id`'s branch: the reply on the
    // other branch (id 5) is not in the transcript.
    assert_eq!(two.turns.len(), 4, "only the selected branch: {:?}", two.turns);
    assert_eq!(two.turns[0].text, "first question");
    assert_eq!(two.turns[1].role, "assistant");
    assert_eq!(two.turns[1].text, "first answer");
    assert_eq!(two.turns[1].thinking, "reasoning");
    assert_eq!(two.turns[1].links.len(), 2);
    assert_eq!(two.turns[1].links[0].title.as_deref(), Some("A"));
    assert_eq!(two.turns[1].links[0].url, "https://a.example");
    assert_eq!(two.turns[1].links[1].title, None);
    assert_eq!(two.turns[1].links[1].url, "https://b.example");
    assert_eq!(two.turns[2].text, "second question");
    assert_eq!(two.turns[3].text, "second answer");
    assert_eq!(two.turns[3].links[0].title.as_deref(), Some("C"));
    assert!(
        !two.turns.iter().any(|turn| turn.text.contains("other branch")),
        "an unselected branch leaked into the transcript"
    );
}

#[test]
fn a_non_export_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-an-export.zip");
    std::fs::write(&path, b"this is not a zip").unwrap();
    let error = read_chat_export(&path).expect_err("a non-export must not import");
    assert!(!error.is_empty(), "the error must be displayable");
}
