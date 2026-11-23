//! SUR-230 slice B: the walking skeleton's rig — an in-process `EngineCore`
//! on a temp data dir, a scripted DeepSeek harness answering every turn
//! (and writing the Captain's `DIGEST.md` on the crew-report wake), and the
//! app's own pages (`ui/shell_fixture.luau`, `ui/helm_live_fixture.luau`)
//! mounted with `captains::install_over`'s host actions on the engine's
//! memory transport. `helm_live_tests.rs` drives it: promote through the
//! sidebar's real menu, add the crew, one crew report, the wake it sets
//! off, and the Needs-you strip and Digest tab over the engine's Drive.
//!
//! The harness reports `HarnessId::DeepSeek`: only DeepSeek runs get the
//! `ZERON_DEEPSEEK_CAPTAINS` stamp (`captain_role_runtime.rs` says why), and
//! the stamp is what files the crew report, wakes the Captain and snapshots
//! `state/needs-you.json`.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value as Json, json};
use tokio::sync::mpsc;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};
use zeron_rpc::{RpcReply, RpcService};
use zgx_script::{ScriptHost, testing::TestApp};

/// The fixture's Captain chat (`helm_live_tests`' `CAP`): the wake turn
/// writes this Captain's `DIGEST.md`.
const CAP: &str = "a1";

/// What the scripted Captain's turn leaves in its Drive on a wake: one ask
/// under the Digest's Needs-you section, for the engine's snapshot and the
/// page's strip.
const DIGEST: &str = "# Billing\n\n## Needs you\n\n- Approve the invoice\n\n## New\n\n## In progress\n";

/// A mounted page over the rig's engine: shell or Digest, same wiring —
/// `mcp_live_tests`' `live` does it for the settings page.
pub struct Rig {
    tmp: tempfile::TempDir,
    data_dir: PathBuf,
    core: EngineCore,
    rt: tokio::runtime::Runtime,
    runs: Arc<Mutex<Vec<RunRequest>>>,
}

fn done(status: DoneStatus) -> AgentEvent {
    AgentEvent::Done {
        status,
        result: None,
        error: None,
        session_id: Some("hs".into()),
    }
}

/// One persistent agent process per `run()`: answers the launch prompt,
/// then parks warm and answers every steer the mailbox delivers —
/// `Agent` in `crates/engine/tests/captain_role_runtime.rs`, plus the
/// `DIGEST.md` write the Captain's Drive gets on a "[crew report]" wake
/// (what a real agent is asked for by the wake's AGENTS.md).
struct Scripted {
    runs: Arc<Mutex<Vec<RunRequest>>>,
    captain: String,
    data_dir: PathBuf,
}

#[async_trait]
impl Harness for Scripted {
    fn id(&self) -> HarnessId {
        HarnessId::DeepSeek
    }
    fn display_name(&self) -> &str {
        "Scripted"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    /// A turn ends exactly on its wire `Done` — no quiesce backstop.
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    /// The real driver's rule (harness/deepseek): a launch-affecting
    /// difference — `env` among them — can never ride the warm path.
    fn same_runtime(&self, live: &RunRequest, next: &RunRequest) -> bool {
        live.model == next.model
            && live.reasoning == next.reasoning
            && zeron_harness::same_model_options(&live.model_options, &next.model_options)
            && live.cwd == next.cwd
            && live.access == next.access
            && live.plan == next.plan
            && live.sandbox == next.sandbox
            && live.mcp == next.mcp
            && live.extra_mcp == next.extra_mcp
            && live.env == next.env
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        mut controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.runs.lock().unwrap().push(request.clone());
        let digest = self
            .data_dir
            .join("captains")
            .join(&self.captain)
            .join("DIGEST.md");
        let (tx, rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(64);
        tokio::spawn(async move {
            let send = |event: AgentEvent| {
                let tx = tx.clone();
                async move { tx.send(Ok(event)).await.is_ok() }
            };
            let _ = send(AgentEvent::SessionStarted {
                harness: HarnessId::DeepSeek,
                model: "mock-deepseek".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "hs".into(),
                assistant_message_id: "a-1".into(),
            })
            .await;
            // The wake: the Captain's turn writes its Digest before it
            // answers, so the engine's turn-end snapshot reads the new list.
            if request.prompt.starts_with("[crew report]") {
                if let Some(dir) = digest.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&digest, DIGEST);
            }
            if !send(AgentEvent::TextDelta {
                text: format!("answered: {}", request.prompt),
            })
            .await
            {
                return;
            }
            if !send(done(DoneStatus::Completed)).await {
                return;
            }
            // Parked between turns: each mailbox steer is a new turn.
            loop {
                tokio::select! {
                    biased;
                    _ = controls.interrupt.cancelled() => {
                        let _ = send(done(DoneStatus::Interrupted)).await;
                        return;
                    }
                    steer = controls.steering.recv() => {
                        let Some(steer) = steer else { return };
                        let boundary = AgentEvent::Steered {
                            assistant_message_id: None,
                            next_assistant_message_id: None,
                        };
                        if !send(boundary).await
                            || !send(AgentEvent::TextDelta {
                                text: format!("answered: {}", steer.prompt),
                            }).await
                            || !send(done(DoneStatus::Completed)).await
                        {
                            return;
                        }
                    }
                }
            }
        });
        Ok(
            futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|e| (e, rx))
            })
            .boxed(),
        )
    }
}

impl Rig {
    pub fn new() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("device-id"), "dev-sur230").unwrap();
        // The roster the engine's CaptainsStore opens at assembly — empty;
        // only the roster RPCs write rows after this.
        std::fs::write(data_dir.join("captains.json"), r#"{"captains":[]}"#).unwrap();
        // Captain Drives live under the DeepSeek home
        // (`AgentAccountsConfig::detect`): point it at this data dir so a
        // Drive is `<data>/captains/<id>`. The engine reads it at assembly
        // and again on every Captains RPC, so it stays set for the rig.
        // SAFETY: a test-process variable, set before the engine exists;
        // nextest runs this case in a process of its own.
        unsafe { std::env::set_var("ZERON_DEEPSEEK_HOME", &data_dir) };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let runs: Arc<Mutex<Vec<RunRequest>>> = Arc::default();
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(Scripted {
            runs: runs.clone(),
            captain: CAP.to_owned(),
            data_dir: data_dir.clone(),
        }));
        // `assemble` boots the workspace host, which spawns
        // (`spawn_blocking`) and needs a Tokio context of its own — as
        // `mcp_live_tests`' `live` enters the runtime it keeps.
        let core = {
            let _enter = rt.enter();
            EngineCore::assemble(&data_dir, Arc::new(registry), HarnessId::DeepSeek, None)
                .expect("engine core assembles")
        };
        core.sessions.set_no_ipc_port();
        // Anchor the ort/glibc link shims below: the onnxruntime archive's
        // references to them only resolve if the shim objects stay in the
        // image, so keep their addresses live here.
        std::hint::black_box(link_shims::symbols());
        Rig {
            tmp,
            data_dir,
            core,
            rt,
            runs,
        }
    }

    /// One RPC on the engine's own service, on the rig's runtime.
    pub fn call(&self, method: &str, params: Json) -> Result<Json, String> {
        self.rt.block_on(async {
            match self.core.rpc_service().handle(method, params).await {
                Ok(RpcReply::Value(value)) => Ok(value),
                Ok(_) => Err(format!("{method} returned a stream")),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    /// A chat as the app's send path makes one: the work folder under the
    /// rig's root, the chat on DeepSeek (the harness `send` reports), then
    /// titled — the wake line names the title.
    pub fn create_chat(&self, chat: &str) {
        let cwd = self.tmp.path().join("work").join(chat);
        std::fs::create_dir_all(&cwd).unwrap();
        self.call(
            zeron_rpc::methods::MUTATE,
            json!({
                "op": "createChat", "chatId": chat, "deviceId": self.core.device_id,
                "cwd": cwd,
                "config": {
                    "harness": "deepseek", "model": "deepseek-flash",
                    "modelOptions": { "webSearch": "off" },
                    "sandbox": "workspace-write", "access": "supervised",
                },
            }),
        )
        .expect("createChat");
        self.call(
            zeron_rpc::methods::MUTATE,
            json!({ "op": "renameChat", "chatId": chat, "title": "Pre-titled" }),
        )
        .expect("renameChat");
    }

    /// The app's own send path: a `Run` command through `QueueCommand`, as
    /// the composer sends it.
    pub fn send(&self, chat: &str, prompt: &str) -> Result<(), String> {
        let cwd = self.tmp.path().join("work").join(chat);
        let request: RunRequest = serde_json::from_value(json!({
            "harness": "deepseek", "prompt": prompt, "cwd": cwd,
            "model": "deepseek-flash", "modelOptions": { "webSearch": "off" },
            "sandbox": "workspace-write", "access": "supervised",
        }))
        .map_err(|e| e.to_string())?;
        let request = serde_json::to_value(&request).map_err(|e| e.to_string())?;
        let command = json!({ "kind": "run", "messageId": prompt, "request": request });
        self.call(
            zeron_rpc::methods::QUEUE_COMMAND,
            json!({ "chatId": chat, "command": command }),
        )
        .map(drop)
    }

    /// The main window's shell fixture, with the Captain host actions over
    /// this rig's engine (the sidebar's Promote menu is the test's way in).
    pub fn shell(&self) -> TestApp {
        self.mount("shell_fixture.luau")
    }

    /// The Digest strip and tab over the same live roster and Drive.
    pub fn digest(&self) -> TestApp {
        self.mount("helm_live_fixture.luau")
    }

    /// A page mount on its own memory client, exactly `mcp_live_tests`'
    /// shape: the host actions `captains::install_over` registers talk to
    /// this rig's engine, not the process-wide one.
    fn mount(&self, page: &str) -> TestApp {
        let client = Rc::new({
            let _enter = self.rt.enter();
            zeron_rpc::memory_client(self.core.rpc_service())
        });
        let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
        let fixture = ui.join(page);
        let _enter = self.rt.enter();
        TestApp::new(move |runtime| {
            // The page's modules name their packages as `ui/.luaurc` does
            // (`@ds`, `@screens`, `@account`); the app's own host registers
            // them, so a fixture mounted on a bare `ScriptHost` must too.
            let host = ScriptHost::new(runtime, fixture)
                .package("ds", ui.join("ds"))
                .package("screens", ui.join("screens"))
                .package("account", ui.join("account"));
            crate::captains::install_over(
                &host,
                move |method: &'static str, params: Json| -> crate::providers::McpCall {
                    let client = Rc::clone(&client);
                    Box::pin(async move {
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/hostcalls.log") {
                            writeln!(f, "{:.3} {method} {params}", std::time::Instant::now().elapsed().as_secs_f64()).ok();
                        }
                        client.call(method, params).await.map_err(|error| error.to_string())
                    })
                },
            );
            host
        })
    }

    /// The roster as `captains.json` holds it (empty when missing or
    /// unparsable), the way the engine's own file reads.
    pub fn captains(&self) -> Vec<Json> {
        let text = std::fs::read_to_string(self.data_dir.join("captains.json")).unwrap_or_default();
        let parsed: Json = serde_json::from_str(&text).unwrap_or(Json::Null);
        parsed["captains"].as_array().cloned().unwrap_or_default()
    }

    /// Every `RunRequest` the scripted harness was dispatched, in order.
    pub fn runs(&self) -> Vec<RunRequest> {
        self.runs.lock().unwrap().clone()
    }

    /// `<data>/captains/<captain>/reports` — the Captain Drive's crew
    /// reports folder.
    pub fn reports_dir(&self, captain: &str) -> PathBuf {
        self.data_dir.join("captains").join(captain).join("reports")
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn close(self) {
        // Shut the engine down while the temp dir still exists: dropping it
        // first removes the files underneath a running engine.
        let Rig { core, rt, tmp, .. } = self;
        rt.block_on(async { core.shutdown().await });
        drop(tmp);
    }
}

/// The cached `ort-sys` archive (`zeron-voice`'s onnxruntime) was built on a
/// newer toolchain than this box provides: it references glibc 2.38's C23
/// `__isoc23_strto*` names, GCC 13's `std::ios_base_library_init`, and
/// libstdc++ GLIBCXX_3.4.31's `basic_string::_M_replace_cold`, none of which
/// glibc 2.36 / libstdc++ 6.0.30 export. Since this module only exists in
/// the test binary, supplying the nine symbols here is the worktree-local
/// fix: each forwards to the older ABI-identical entry or mirrors
/// libstdc++'s own implementation.
mod link_shims {
    use std::ffi::{c_char, c_int, c_long, c_longlong, c_ulong, c_ulonglong, c_void};
    use std::ptr;
    use std::sync::Once;
    use std::sync::atomic::AtomicUsize;

    unsafe extern "C" {
        fn strtol(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_long;
        fn strtoul(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_ulong;
        fn strtoll(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_longlong;
        fn strtoull(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_ulonglong;
        fn strtoll_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int, loc: *mut c_void) -> c_longlong;
        fn strtoull_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int, loc: *mut c_void) -> c_ulonglong;
        /// `std::ios_base::Init::Init()` — what GCC 13's
        /// `ios_base_library_init` replaced; libstdc++ 6.0.30 exports it.
        #[link_name = "_ZNSt8ios_base4InitC1Ev"]
        fn ios_base_init_ctor(this: *mut c_void);
    }

    // The C23 strto* variants are the same functions under new names
    // (glibc 2.38 renamed them for the C23 `strtol`-family prototypes).
    #[unsafe(export_name = "__isoc23_strtol")]
    pub extern "C" fn isoc23_strtol(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_long {
        unsafe { strtol(nptr, endptr, base) }
    }
    #[unsafe(export_name = "__isoc23_strtoul")]
    pub extern "C" fn isoc23_strtoul(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_ulong {
        unsafe { strtoul(nptr, endptr, base) }
    }
    #[unsafe(export_name = "__isoc23_strtoll")]
    pub extern "C" fn isoc23_strtoll(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_longlong {
        unsafe { strtoll(nptr, endptr, base) }
    }
    #[unsafe(export_name = "__isoc23_strtoull")]
    pub extern "C" fn isoc23_strtoull(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> c_ulonglong {
        unsafe { strtoull(nptr, endptr, base) }
    }
    #[unsafe(export_name = "__isoc23_strtoll_l")]
    pub extern "C" fn isoc23_strtoll_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int, loc: *mut c_void) -> c_longlong {
        unsafe { strtoll_l(nptr, endptr, base, loc) }
    }
    #[unsafe(export_name = "__isoc23_strtoull_l")]
    pub extern "C" fn isoc23_strtoull_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int, loc: *mut c_void) -> c_ulonglong {
        unsafe { strtoull_l(nptr, endptr, base, loc) }
    }

    /// `std::__cxx11::basic_string::_M_replace_cold` (GLIBCXX_3.4.31) — the
    /// in-place tail of `_M_replace` when `s` may overlap the buffer,
    /// verbatim from `bits/basic_string.tcc`: `_S_move` is memmove
    /// (`ptr::copy`), `_S_copy` is memcpy (`ptr::copy_nonoverlapping`).
    /// The member's `this` only reaches static traits helpers, so the
    /// receiver pointer goes unused.
    unsafe fn m_replace_cold<T: Copy>(p: *mut T, len1: usize, s: *const T, len2: usize, how_much: usize) {
        let size = size_of::<T>();
        if len2 != 0 && len2 <= len1 {
            unsafe { ptr::copy(s, p, len2) };
        }
        if how_much != 0 && len1 != len2 {
            unsafe { ptr::copy(p.add(len1), p.add(len2), how_much) };
        }
        if len2 > len1 {
            if (s as usize) + len2 * size <= (p as usize) + len1 * size {
                unsafe { ptr::copy(s, p, len2) };
            } else if (s as usize) >= (p as usize) + len1 * size {
                let poff = ((s as usize) - (p as usize)) / size + (len2 - len1);
                unsafe { ptr::copy_nonoverlapping(p.add(poff), p, len2) };
            } else {
                let nleft = ((p as usize) + len1 * size - (s as usize)) / size;
                unsafe { ptr::copy(s, p, nleft) };
                unsafe { ptr::copy_nonoverlapping(p.add(len2), p.add(nleft), len2 - nleft) };
            }
        }
    }

    /// `basic_string<char>` — mangled `…_M_replace_coldEPcmPKcmm`.
    #[unsafe(export_name = "_ZNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEE15_M_replace_coldEPcmPKcmm")]
    pub extern "C" fn m_replace_cold_char(_this: *mut c_void, p: *mut u8, len1: usize, s: *const u8, len2: usize, how_much: usize) {
        unsafe { m_replace_cold(p, len1, s, len2, how_much) }
    }

    /// `basic_string<wchar_t>` — `…_M_replace_coldEPwmPKwmm`; `wchar_t` is
    /// four bytes on Linux.
    #[unsafe(export_name = "_ZNSt7__cxx1112basic_stringIwSt11char_traitsIwESaIwEE15_M_replace_coldEPwmPKwmm")]
    pub extern "C" fn m_replace_cold_wchar(_this: *mut c_void, p: *mut u32, len1: usize, s: *const u32, len2: usize, how_much: usize) {
        unsafe { m_replace_cold(p, len1, s, len2, how_much) }
    }

    /// `std::ios_base_library_init()` (GLIBCXX_3.4.32) — GCC 13's <iostream>
    /// static-init hook. libstdc++ runs a function-local
    /// `static ios_base::Init` once; `Once` is the same one-shot through the
    /// older ctor. `Init` is an empty class — the ctor only touches static
    /// state, so any valid `this` address serves.
    #[unsafe(export_name = "_ZSt21ios_base_library_initv")]
    pub extern "C" fn ios_base_library_init() {
        static ONCE: Once = Once::new();
        static SLOT: AtomicUsize = AtomicUsize::new(0);
        ONCE.call_once(|| unsafe { ios_base_init_ctor(&SLOT as *const AtomicUsize as *mut c_void) });
    }

    /// Every shim, as addresses: `Rig::new` black-boxes this so the linker
    /// always keeps the shim objects in the image for the archive to
    /// resolve against.
    pub fn symbols() -> [usize; 9] {
        [
            isoc23_strtol as usize,
            isoc23_strtoul as usize,
            isoc23_strtoll as usize,
            isoc23_strtoull as usize,
            isoc23_strtoll_l as usize,
            isoc23_strtoull_l as usize,
            m_replace_cold_char as usize,
            m_replace_cold_wchar as usize,
            ios_base_library_init as usize,
        ]
    }
}

// TEMPORARY PROBE — micro-step kor2's press; remove before done.
#[test]
fn probe_verbatim() {
    use zgx::input::{InputEvent, PointerButton};
    let log = std::env::temp_dir().join("probe_focus5.log");
    let _ = std::fs::remove_file(&log);
    let w = |m: String| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&log).unwrap();
        writeln!(f, "{m}").unwrap();
    };
    let rig = crate::helm_live::Rig::new();
    rig.create_chat("a1");
    rig.create_chat("a2");
    let mut shell = rig.shell();
    shell.ui.scene.borrow_mut().resize(1200., 800.);
    shell.frames(60);
    let row = "#shell-sidebar >> role:button \"Fix the build\"";
    crate::focus_tests::keys_on_row(&mut shell, row);
    shell.keys("shift+F10");
    shell.wait_until(|app| app.find("role:menuitem \"Promote to Captain…\"").is_some());
    shell.click("role:menuitem \"Promote to Captain…\"");
    shell.wait_until(|app| app.find("role:textbox \"Name\"").is_some());
    shell.frames(30);
    shell.fill("role:textbox \"Name\"", "Billing");
    shell.click("role:button \"Promote to Captain\"");
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(10) {
        if rig.captains().iter().any(|c| c["chatId"] == "a1") { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // kor2, micro-stepped: target, PointerDown, PointerUp, settle, frames.
    let b = shell.bounds(row);
    let (x, y) = (b.x + b.width / 2., b.y + b.height / 2.);
    w(format!("bounds={b:?} dialog={:?} focused={:?} scope={:?}",
        shell.find("role:dialog"), shell.ui.input.focused(), shell.ui.input.focus_scope()));
    shell.ui.dispatch(InputEvent::PointerDown { x, y, button: PointerButton::Primary });
    w(format!("after down: focused={:?} dialog={:?} scope={:?}", shell.ui.input.focused(), shell.find("role:dialog"), shell.ui.input.focus_scope()));
    shell.ui.dispatch(InputEvent::PointerUp { x, y, button: PointerButton::Primary });
    shell.settle();
    w(format!("after up: focused={:?} scope={:?}", shell.ui.input.focused(), shell.ui.input.focus_scope()));
    shell.frames(10);
    let t = shell.target(row).unwrap();
    w(format!("after frames: focused={:?} target={t:?} dialog={:?} scope={:?}", shell.ui.input.focused(), shell.find("role:dialog"), shell.ui.input.focus_scope()));
    let ok = shell.ui.input.focus(&shell.ui.scene, Some(t));
    w(format!("focus -> {ok} focused={:?}", shell.ui.input.focused()));
    rig.close();
}
