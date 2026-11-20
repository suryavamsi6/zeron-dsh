//! Settings → MCP servers against a real engine (SUR-145): the page mounted
//! from `ui/mcp_live_fixture.luau` (no `settings_stub`) with
//! `providers::install_mcp`'s own two host actions over
//! `zeron_rpc::memory_client` of an in-process `EngineCore`. The four
//! assertions are the contract `ui/tests/mcp_live.test.luau` names, read from
//! the engine's own file, `<data_dir>/mcp-servers.json`:
//!
//! 1. add through the page → the server is in the engine's file;
//! 2. a second add leaves the first's `env` byte-identical;
//! 3. removing the first leaves only the second, its `env` unchanged;
//! 4. a refused save (a 17th server) shows the engine's words and leaves
//!    the file untouched.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{Value as Json, json};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::MockHarness;
use zeron_proto::HarnessId;
use zgx_script::{ScriptHost, testing::TestApp};

const PAGE: &str = "#settings-page";

/// A mounted page over its own engine on a temp data dir, and the runtime
/// the engine's in-memory transport was spawned on (kept for `shutdown`).
struct Live {
    app: TestApp,
    core: EngineCore,
    data_dir: PathBuf,
    _dir: tempfile::TempDir,
    rt: tokio::runtime::Runtime,
}

impl Live {
    fn close(self) {
        let Live { core, rt, .. } = self;
        rt.block_on(async { core.shutdown().await });
    }
}

fn live(preload: &[Json]) -> Live {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    if !preload.is_empty() {
        // The engine's own file shape (`user_mcp`): written directly so the
        // page loads it through `McpServers` without 16 adds first.
        let payload = json!({ "servers": preload });
        std::fs::write(
            data_dir.join("mcp-servers.json"),
            serde_json::to_vec_pretty(&payload).unwrap(),
        )
        .unwrap();
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness { script: Vec::new() }));
    // `assemble` boots the workspace host, which spawns (`spawn_blocking`)
    // and needs a Tokio context of its own — as the engine's tests get from
    // `#[tokio::test]`; this plain test enters the runtime it keeps.
    let core = {
        let _enter = rt.enter();
        EngineCore::assemble(&data_dir, Arc::new(registry), HarnessId::Mock, None)
            .expect("engine core assembles")
    };
    core.sessions.set_no_ipc_port();
    let client = Rc::new({
        let _enter = rt.enter();
        zeron_rpc::memory_client(core.rpc_service())
    });
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/mcp_live_fixture.luau");
    let app = {
        let _enter = rt.enter();
        TestApp::new(move |runtime| {
            let host = ScriptHost::new(runtime, fixture);
            crate::providers::install_mcp(&host, {
                let client = Rc::clone(&client);
                move |method, params| {
                    let client = Rc::clone(&client);
                    Box::pin(async move {
                        client
                            .call(method, params)
                            .await
                            .map_err(|error| error.to_string())
                    })
                }
            });
            host
        })
    };
    Live {
        app,
        core,
        data_dir,
        _dir: dir,
        rt,
    }
}

fn servers(data_dir: &Path) -> Vec<Json> {
    let text = std::fs::read_to_string(data_dir.join("mcp-servers.json")).unwrap_or_default();
    let parsed: Json = serde_json::from_str(&text).unwrap_or(Json::Null);
    parsed["servers"].as_array().cloned().unwrap_or_default()
}

fn env_text(servers: &[Json], name: &str) -> Option<String> {
    servers
        .iter()
        .find(|server| server["name"] == name)
        .map(|server| serde_json::to_string(&server["env"]).unwrap())
}

fn names(servers: &[Json]) -> Vec<String> {
    servers
        .iter()
        .map(|server| server["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn has(app: &mut TestApp, target: &str) -> bool {
    app.find(target).is_some()
}

fn wait_for(app: &mut TestApp, target: &str) -> bool {
    app.wait_until(|app| has(app, target))
}

fn fill(app: &mut TestApp, field: &str, text: &str) {
    app.fill(&format!("{PAGE} >> role:textbox \"{field}\""), text);
}

fn add(app: &mut TestApp, name: &str, command: &str, args: &str, env: &str) {
    fill(app, "Name", name);
    fill(app, "Command", command);
    fill(app, "Arguments", args);
    fill(app, "Environment", env);
    app.click(&format!("{PAGE} >> role:button \"Save\""));
}

fn listed(app: &mut TestApp, name: &str) -> bool {
    wait_for(app, &format!("{PAGE} >> role:button \"Remove {name}\""))
}

#[test]
fn adding_a_server_writes_it_to_the_engines_list() {
    let mut live = live(&[]);
    assert!(
        wait_for(&mut live.app, &format!("{PAGE} >> \"No MCP servers yet\"")),
        "the page never listed the engine's empty list"
    );
    add(
        &mut live.app,
        "notes",
        "npx",
        "-y\n@modelcontextprotocol/server-notes\n/work",
        "NOTES_ROOT=/work/notes\nTOKEN=abc=def ghi",
    );
    assert!(listed(&mut live.app, "notes"), "the saved server did not list");

    let list = servers(&live.data_dir);
    assert_eq!(list.len(), 1, "{list:?}");
    let server = &list[0];
    assert_eq!(server["name"], "notes");
    assert_eq!(server["command"], "npx");
    assert_eq!(
        server["args"],
        json!(["-y", "@modelcontextprotocol/server-notes", "/work"])
    );
    assert_eq!(server["env"]["NOTES_ROOT"], "/work/notes");
    assert_eq!(
        server["env"]["TOKEN"], "abc=def ghi",
        "a value with an inner = and a space is kept as typed"
    );
    live.close();
}

#[test]
fn a_second_add_leaves_the_firsts_env_byte_identical() {
    let mut live = live(&[]);
    assert!(wait_for(
        &mut live.app,
        &format!("{PAGE} >> \"No MCP servers yet\"")
    ));
    add(
        &mut live.app,
        "notes",
        "npx",
        "-y\nserver-notes\n/work",
        "TOKEN=abc=def ghi\nNOTES_ROOT=/work/notes",
    );
    assert!(listed(&mut live.app, "notes"));
    let first_env = env_text(&servers(&live.data_dir), "notes").expect("the first server's env");

    add(&mut live.app, "second", "/usr/bin/second", "", "SECOND=value-two");
    assert!(listed(&mut live.app, "second"), "the second server did not list");

    let list = servers(&live.data_dir);
    assert_eq!(names(&list), vec!["notes", "second"], "the engine's order");
    assert_eq!(
        env_text(&list, "notes").as_deref(),
        Some(first_env.as_str()),
        "the first server's env came back byte-identical through the second save"
    );
    assert_eq!(
        env_text(&list, "second").as_deref(),
        Some(r#"{"SECOND":"value-two"}"#),
        "the second's env is what was typed"
    );
    live.close();
}

#[test]
fn removing_the_first_leaves_only_the_second_unchanged() {
    let mut live = live(&[]);
    assert!(wait_for(
        &mut live.app,
        &format!("{PAGE} >> \"No MCP servers yet\"")
    ));
    add(&mut live.app, "notes", "npx", "-y\nserver-notes", "TOKEN=abc=def ghi");
    assert!(listed(&mut live.app, "notes"));
    add(&mut live.app, "second", "/usr/bin/second", "", "SECOND=value-two");
    assert!(listed(&mut live.app, "second"));
    let second_env = env_text(&servers(&live.data_dir), "second").expect("the second's env");

    live.app
        .click(&format!("{PAGE} >> role:button \"Remove notes\""));
    assert!(
        wait_for(&mut live.app, "role:dialog \"Remove “notes”?\""),
        "removing asked nothing"
    );
    live.app.click("role:button \"Remove server\"");
    assert!(
        live.app
            .wait_until(|app| !has(app, &format!("{PAGE} >> role:button \"Remove notes\""))),
        "the removed server still lists"
    );

    let list = servers(&live.data_dir);
    assert_eq!(names(&list), vec!["second"], "only the second remains");
    assert_eq!(
        env_text(&list, "second").as_deref(),
        Some(second_env.as_str()),
        "the kept server's env is unchanged"
    );
    live.close();
}

#[test]
fn a_refused_save_shows_the_engines_words_and_leaves_the_file_untouched() {
    let preload: Vec<Json> = (0..16)
        .map(|i| {
            json!({
                "name": format!("s{i}"),
                "command": "x",
                "args": [],
                "env": { "K": format!("v{i}") },
            })
        })
        .collect();
    let mut live = live(&preload);
    assert!(
        live.app
            .wait_until(|app| has(app, &format!("{PAGE} >> role:button \"Remove s15\""))),
        "the page did not list the engine's 16 servers"
    );
    let before = std::fs::read(live.data_dir.join("mcp-servers.json")).unwrap();

    add(&mut live.app, "seventeenth", "x", "", "");
    assert!(
        wait_for(&mut live.app, &format!("{PAGE} >> \"At most 16 MCP servers\"")),
        "the engine's words never reached the page"
    );
    let after = std::fs::read(live.data_dir.join("mcp-servers.json")).unwrap();
    assert_eq!(before, after, "a refused save wrote the file");
    assert_eq!(servers(&live.data_dir).len(), 16);
    assert!(
        !has(&mut live.app, &format!("{PAGE} >> role:button \"Remove seventeenth\"")),
        "the refused server was drawn as if saved"
    );
    live.close();
}
