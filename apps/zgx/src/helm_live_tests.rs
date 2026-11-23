//! SUR-230's walking skeleton: the Helm loop end to end on real zgx
//! widgets over an in-process engine with scripted turns — promotion
//! through the sidebar's own menu, the crew's turn through the app's own
//! `QueueCommand`, the scripted Helm wake the crew's report sets off, and
//! the Needs-you strip and Digest tab over the engine's own Drive.
//! `crate::helm_live` is the rig this file's API defines; slice B
//! implements it.

use std::time::Duration;

use serde_json::json;
use zgx_script::testing::TestApp;

/// The fixture's two chats, both in project Atlas: a1 "Fix the build" (the
/// chat promoted to Captain) and a2 "Write the docs" (its crew).
const CAP: &str = "a1";
const CREW: &str = "a2";

/// Real time until `done`: the engine answers on threads of its own, so a
/// frame pump alone can't wait a roster entry, a run or a file in.
fn wait(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

/// Open `row`'s context menu. A row that already holds the keyboard (a closed
/// dialog restores focus to it) cannot take it again — `keys_on_row` asserts
/// the click moved focus — so there the menu is asked for directly.
fn menu(app: &mut TestApp, row: &str) {
    // A dialog or menu still leaving holds its focus scope until its exit
    // animation lands (the pop runs on the frames it takes): wait that out
    // first — mid-exit `focused()` still names the panel, not where the keys
    // are going.
    app.wait_until(|app| app.ui.input.focus_scope().is_none());
    let target = app.target(row).unwrap_or_else(|error| panic!("{row}: {error:?}"));
    if app.ui.input.focused() != Some(target) {
        crate::focus_tests::keys_on_row(app, row);
    } else {
        app.frames(2);
    }
    app.keys("shift+F10");
    assert!(
        app.wait_until(|app| app.find("role:menuitem \"Rename\"").is_some()),
        "the menu never opened on {row}; page:\n{}",
        app.tree()
    );
    app.frames(4);
}

#[test]
fn helm_live_promote_add_report_and_needs_you() {
    let rig = crate::helm_live::Rig::new();
    rig.create_chat(CAP);
    rig.create_chat(CREW);

    // 1. Promotion through the real sidebar menu over the in-process engine.
    let mut shell = rig.shell();
    shell.ui.scene.borrow_mut().resize(1200., 800.);
    shell.frames(60);
    let row = "#shell-sidebar >> role:button \"Fix the build\"";
    menu(&mut shell, row);
    assert!(
        shell.wait_until(|app| app.find("role:menuitem \"Promote to Captain…\"").is_some()),
        "the chat's menu never offered Promote; page:\n{}",
        shell.tree()
    );
    shell.click("role:menuitem \"Promote to Captain…\"");
    assert!(
        shell.wait_until(|app| app.find("role:textbox \"Name\"").is_some()),
        "the promotion dialog never opened"
    );
    shell.frames(30);
    shell.fill("role:textbox \"Name\"", "Billing");
    shell.click("role:button \"Promote to Captain\"");
    wait("the engine's roster to record the promotion", || {
        rig.captains().iter().any(|c| c["chatId"] == CAP && c["name"] == "Billing")
    });
    // The widget read the roster back: the same menu now offers the way back.
    menu(&mut shell, row);
    assert!(
        shell.wait_until(|app| app.find("role:menuitem \"Demote to chat\"").is_some()),
        "the menu still offered Promote after the engine took it; page:\n{}",
        shell.tree()
    );
    shell.key(zgx::input::Key::Escape);
    shell.frames(10);

    // 2. The crew chat joins through its own menu (SUR-91): Move to Captain →
    // the Captain just promoted. Same host bridge, same engine — a real widget.
    let crew_row = "#shell-sidebar >> role:button \"Write the docs\"";
    menu(&mut shell, crew_row);
    shell.click("role:menuitem \"Move to Captain\"");
    assert!(
        shell.wait_until(|app| app.find("role:menuitem \"Billing\"").is_some()),
        "Move to Captain offered no Captains; page:\n{}", shell.tree()
    );
    shell.frames(4);
    shell.click("role:menuitem \"Billing\"");
    // The AddCrew the menu sends rides the page's own executor (ui.call ->
    // install_over -> the rig's memory client): `wait` only sleeps and would
    // starve it, so the roster is watched on the app — its settles and frames
    // are what carry the call to the engine.
    assert!(
        shell.wait_until(|_| rig
            .captains()
            .iter()
            .any(|c| c["chatId"] == CAP && c["crew"] == json!([CREW]))),
        "the crew row never reached the engine's roster"
    );

    // 3. One crew report: the app's own send path (QueueCommand -> run), the
    // scripted harness answers, and the engine files the report in the Captain's
    // Drive.
    rig.send(CREW, "hold the line").expect("the engine takes the crew turn");
    let report = rig.reports_dir(CAP).join(format!("{CREW}.md"));
    wait("the crew report in the Captain's Drive", || {
        report.is_file()
            && std::fs::read_to_string(&report).is_ok_and(|body| body.contains("answered: hold the line"))
    });
    let report = std::fs::read_to_string(&report).expect("the report reads");
    assert!(
        report.starts_with(&format!("# Crew report: {CREW}\n")),
        "the report names the crew chat: {report:?}"
    );

    // 4. One Helm wake: the Captain's own run, carrying the crew's report line,
    // and the scripted turn writes the DIGEST the engine snapshots into
    // needs-you.json.
    wait("the Captain's wake run", || rig.runs().len() >= 2);
    let runs = rig.runs();
    assert_eq!(runs.len(), 2, "exactly one crew turn and one wake");
    let wake = &runs[1].prompt;
    assert!(wake.starts_with("[crew report] Pre-titled ("), "the wake is the report line; saw {wake:?}");
    assert!(wake.contains("): completed"), "the wake carries the turn's status; saw {wake:?}");
    assert!(wake.contains("answered: hold the line"), "the wake carries the crew's last reply; saw {wake:?}");
    let needs_you = rig.data_dir().join("captains").join(CAP).join("state/needs-you.json");
    wait("the engine's needs-you snapshot", || {
        needs_you.is_file()
            && std::fs::read_to_string(&needs_you).is_ok_and(|body| body.contains("Approve the invoice"))
    });

    // 5. The Needs-you ask on screen: the real Strip and Digest tab over the live
    // engine's own Drive. The strip mounts hidden (a "· 0" placeholder) before
    // CaptainDrive answers, so it is its count that is waited for, not the node.
    let mut digest = rig.digest();
    digest.ui.scene.borrow_mut().resize(900., 900.);
    assert!(
        digest.wait_until(|app| app.tree().contains("Needs you · 1")),
        "the Needs-you strip never showed; page:\n{}",
        digest.tree()
    );
    let tree = digest.tree();
    assert!(tree.contains("Needs you · 1"), "the strip's count; page:\n{tree}");
    assert!(tree.contains("Approve the invoice"), "the ask itself, from the Digest tab; page:\n{tree}");

    rig.close();
}
