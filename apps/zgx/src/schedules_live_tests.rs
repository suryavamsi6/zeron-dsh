//! Settings → Automation tasks over the engine's own answer (SUR-177): a
//! task written through the real `ScheduleSave` RPC of an in-process engine
//! (`helm_live::Rig`), listed back through `Schedules`, and that answer —
//! not a hand-shaped copy — injected into the shell fixture's stub. The
//! page must draw the task's name and its next run; if it renders the empty
//! list while the engine holds a task, this test fails.

use serde_json::json;
use zgx_script::{ScriptHost, testing::TestApp};

/// The shell as the Luau suites mount it, sized as a settings window.
fn mount() -> TestApp {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/shell_fixture.luau");
    let app = TestApp::new(|runtime| ScriptHost::new(runtime, path));
    app.ui.scene.borrow_mut().resize(1200., 900.);
    app.frames(30);
    app
}

#[test]
fn the_automation_tasks_page_renders_a_real_stored_schedule() {
    let rig = crate::helm_live::Rig::new();
    // The engine's own write path validates the rule and recomputes
    // `nextAt`: this answer is what a page in the field actually gets.
    rig.call(
        zeron_rpc::methods::SCHEDULE_SAVE,
        json!({
            "task": {
                "id": "",
                "title": "Standup summary",
                "prompt": "Write the standup summary",
                "rrule": "DTSTART:20261101T090000Z\nRRULE:FREQ=DAILY;BYHOUR=9;BYMINUTE=0",
                "target": { "kind": "new", "cwd": "/proj" },
                "active": true,
            }
        }),
    )
    .expect("the schedule saves");
    let answer = rig
        .call(zeron_rpc::methods::SCHEDULES, json!({}))
        .expect("the schedules list");
    assert_eq!(
        answer.get("tasks").and_then(|tasks| tasks.as_array()).map(|tasks| tasks.len()),
        Some(1),
        "the engine holds the one task: {answer}"
    );
    // The shot harness's evidence file (`=<state>:@<path>`): the same
    // answer, on disk, for the audit_shots render cited beside it.
    let proof = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("schedules-live-proof.json");
    std::fs::write(&proof, serde_json::to_vec_pretty(&answer).unwrap()).unwrap();

    let mut app = mount();
    app.host
        .set_state("settings_stub_schedule_tasks", crate::plugins::json_value(&answer))
        .expect("the stub takes the engine's answer");
    assert!(app.host.run_command("settings_stub.sign_in"));
    assert!(app.host.run_command("shell.settings"));
    app.frames(20);
    app.target("#settings-nav >> role:tab \"Automation tasks\"")
        .expect("the Automation tasks tab");
    app.click("#settings-nav >> role:tab \"Automation tasks\"");
    for _ in 0..100 {
        app.frames(1);
        if app.tree().contains("Standup summary") {
            break;
        }
    }
    let tree = app.tree();
    assert!(tree.contains("Standup summary"), "the task's name is on the page:\n{tree}");
    assert!(tree.contains("Every day at 09:00 UTC"), "its rule in words:\n{tree}");
    assert!(tree.contains("Next"), "its next run is on the page:\n{tree}");
    assert!(
        !tree.contains("No automation tasks yet"),
        "the engine holds a task: the empty state must not render:\n{tree}"
    );
}
