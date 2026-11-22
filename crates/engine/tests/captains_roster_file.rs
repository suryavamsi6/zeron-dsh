//! SUR-191: a malformed `captains.json` must not stop the engine.
//!
//! The roster is device-local engine state. One unreadable file must leave every
//! ordinary chat running unstamped — the dispatch path already tolerates a
//! malformed registry (`if let Ok(registry) = Registry::load(..)`), so a startup
//! that dies on the same file is the two paths disagreeing. The file itself is
//! never rewritten by a failed open or a refused mutation, and a repaired file
//! is written normally on the next mutation, so the degrade is not a latch.

use std::sync::Arc;

use zeron_engine::{EngineCore, EngineProfile, HarnessId, default_registry};

fn assemble(dir: &std::path::Path) -> EngineCore {
    EngineCore::assemble_with_profile(
        EngineProfile::local(dir).expect("local profile"),
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .expect("a malformed roster must not stop the engine")
}

#[tokio::test]
async fn a_malformed_roster_does_not_stop_the_engine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let roster = dir.path().join("captains.json");
    let malformed = b"{\"captains\": [";
    std::fs::write(&roster, malformed).expect("write a malformed roster");

    // 1. Startup survives and serves an empty roster, and the file is untouched.
    let core = assemble(dir.path());
    assert!(
        core.captains.snapshot().captains().is_empty(),
        "an unreadable roster serves an empty one"
    );
    assert_eq!(
        std::fs::read(&roster).expect("read the roster"),
        malformed,
        "a failed open must not rewrite the roster file"
    );

    // 2. A mutation is refused with a message that names the file, and still
    //    leaves it byte-identical — a corrupt roster is never overwritten by an
    //    empty one.
    let refused = core
        .captains
        .promote("cap-a", "Ada")
        .expect_err("a mutation over an unreadable roster must be refused");
    assert!(
        refused.contains("captains.json"),
        "the refusal must name the roster file: {refused}"
    );
    assert_eq!(
        std::fs::read(&roster).expect("read the roster"),
        malformed,
        "a refused mutation must not rewrite the roster file"
    );

    // 3. Once the file is repaired the next write goes through: the degrade is
    //    not a permanent latch.
    std::fs::write(&roster, b"{\"captains\": []}").expect("repair the roster");
    core.captains
        .promote("cap-a", "Ada")
        .expect("a repaired roster accepts a write");
    assert!(
        core.captains.snapshot().is_captain("cap-a"),
        "the repaired write landed"
    );
}
