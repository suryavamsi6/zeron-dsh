//! SUR-228 (red): a packaged build provisions `dsh-captains` into the `sdk`
//! profile, with no pnpm and no promote-time install, and never at a user's
//! expense.
//!
//! The measured failure this covers: a copied `DSH_HOME/profiles/sdk` whose
//! bundle link dangles logs `dsh: skipping profile bundle
//! "zeron-dsh-captains": … cannot resolve` and then runs a healthy-looking
//! turn with zero `apply` calls — no role section, no Drive tools, no error.
//! The profile is therefore part of provisioning, not of installation: the
//! prepare step every DeepSeek run passes must leave it readable, writable
//! and listing the bundle.
//!
//! Everything here drives `PinRunEnvironment::prepare` — the path a real run
//! takes — rather than a helper, so the case fails if the hook is dropped.
//! `ZERON_CAPTAINS_PLUGIN_DIR` stands in for the packaged layout, which is
//! what `plugin_dir()` resolves from; nextest runs one test per process, so
//! setting it per test is safe.

use std::path::{Path, PathBuf};

use zeron_engine::agent_accounts::{AgentAccounts, AgentAccountsConfig, PinRunEnvironment};
use zeron_engine::registry::RunEnvironment;
use zeron_engine::PLUGIN_DIR_ENV;
use zeron_harness::deepseek::plugins::installed_bundles;
use zeron_proto::HarnessId;

const BUNDLE: &str = "zeron-dsh-captains";
const SDK_TEMPLATE: [&str; 2] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-sdk-app"];

/// The checkout's bundle, two directories up from `crates/engine`.
fn bundle_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/dsh-captains")
}

/// The layout a package ships: `dsh-plugins/dsh-captains` beside the binary.
fn stage_bundle(root: &Path) -> PathBuf {
    let staged = root.join("dsh-plugins/dsh-captains");
    std::fs::create_dir_all(&staged).expect("the staged bundle directory should exist");
    for name in ["package.json", "index.js", "cordis.patch.yml"] {
        std::fs::copy(bundle_source().join(name), staged.join(name))
            .unwrap_or_else(|error| panic!("the bundle should ship {name}: {error}"));
    }
    staged
}

struct Rig {
    _tmp: tempfile::TempDir,
    data_dir: PathBuf,
    home: PathBuf,
    bundle: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("a temp dir should hold the rig");
        let bundle = stage_bundle(tmp.path());
        // The process owns its environment (nextest runs one test per process).
        unsafe { std::env::set_var(PLUGIN_DIR_ENV, &bundle) };
        Self {
            data_dir: tmp.path().join("data"),
            home: tmp.path().join("deepseek"),
            bundle,
            _tmp: tmp,
        }
    }

    fn profile(&self) -> PathBuf {
        self.home.join("profiles").join("sdk")
    }

    fn manifest(&self) -> String {
        std::fs::read_to_string(self.profile().join("package.json"))
            .expect("the profile's package.json should read")
    }

    fn manifest_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.manifest()).expect("the manifest should be JSON")
    }

    /// One run's prepare step, exactly as dispatch calls it.
    async fn prepare(&self, chat_id: Option<&str>) -> Result<(), String> {
        let mut request: zeron_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "hello", "cwd": ".", "sandbox": "workspace-write"
        }))
        .expect("a minimal request should parse");
        let env = PinRunEnvironment(AgentAccounts::new(AgentAccountsConfig {
            data_dir: self.data_dir.clone(),
            deepseek_home: self.home.clone(),
        }));
        env.prepare(HarnessId::DeepSeek, chat_id, &mut request)
            .await
    }

    /// The bundle dsh would load: listed in the profile manifest *and*
    /// resolvable from the profile's own `node_modules` — either as a link at
    /// the packaged directory or as a materialised copy of it, because the
    /// link primitive is not available everywhere (Windows without Developer
    /// Mode has no unprivileged symlink).
    fn provisioned(&self) -> bool {
        let manifest = self.manifest_json();
        let listed = manifest["dsh"]["profile"]["bundles"]
            .as_array()
            .is_some_and(|bundles| bundles.iter().any(|b| b == BUNDLE));
        listed && self.resolves_bundle()
    }

    /// The profile's `node_modules` entry holds the packaged bundle: a link
    /// that canonicalises to it, or a copy that names itself.
    fn resolves_bundle(&self) -> bool {
        let entry = self.profile().join("node_modules").join(BUNDLE);
        let linked = std::fs::canonicalize(&entry)
            .ok()
            .zip(std::fs::canonicalize(&self.bundle).ok())
            .is_some_and(|(entry, staged)| entry == staged);
        let materialised = std::fs::read_to_string(entry.join("package.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|manifest| manifest["name"].as_str().map(str::to_owned))
            .is_some_and(|name| name == BUNDLE);
        linked || materialised
    }
}

/// Every non-empty line of `before` still appears in `after`, in order:
/// nothing an existing profile held was dropped or rewritten.
fn preserved_in_order(before: &str, after: &str) -> bool {
    let mut rest = after;
    before
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .all(|line| {
            let Some(at) = rest.find(line) else {
                return false;
            };
            rest = &rest[at + line.len()..];
            true
        })
}

/// A byte-for-byte snapshot of a materialised bundle tree, with relative
/// paths normalised to `/` so the comparison is portable.
fn snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        let mut entries = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("{} should read: {error}", dir.display()))
            .collect::<Result<Vec<_>, _>>()
            .expect("the tree's entries should read");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let file_type = entry
                .file_type()
                .unwrap_or_else(|error| panic!("{} should stat: {error}", entry.path().display()));
            if file_type.is_dir() {
                walk(&entry.path(), root, files);
            } else if file_type.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .expect("a recursive entry is below its root")
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((
                    relative,
                    std::fs::read(entry.path()).unwrap_or_else(|error| {
                        panic!("{} should read: {error}", entry.path().display())
                    }),
                ));
            }
        }
    }

    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    files
}

/// Rule 2 (and the acceptance's first clause): a clean home gets a profile
/// created from the shipped template, with the packaged bundle materialised
/// and listed — no pnpm, no promote-time install, and writable, because dsh
/// rewrites `cordis.yml` at startup.
#[tokio::test]
async fn a_clean_home_is_provisioned_with_the_packaged_bundle() {
    let rig = Rig::new();
    assert!(!rig.profile().exists(), "the rig should start with no profile");

    rig.prepare(None).await.expect("prepare should provision the profile");

    assert!(
        rig.provisioned(),
        "the profile should list and resolve {BUNDLE}; manifest: {}",
        rig.manifest()
    );
    let manifest = rig.manifest_json();
    let bundles: Vec<&str> = manifest["dsh"]["profile"]["bundles"]
        .as_array()
        .expect("the bundles list should be an array")
        .iter()
        .filter_map(|b| b.as_str())
        .collect();
    for shipped in SDK_TEMPLATE {
        assert!(
            bundles.contains(&shipped),
            "the sdk template's {shipped} should survive provisioning; saw {bundles:?}"
        );
    }
    assert!(
        rig.profile().join("cordis.patch.yml").is_file(),
        "the profile should carry the user patch layer dsh expects"
    );
    // dsh rewrites cordis.yml at startup: a profile it cannot write aborts the
    // launch (EACCES … open profiles/sdk/cordis.yml).
    std::fs::write(rig.profile().join("writable.probe"), b"ok")
        .expect("a provisioned profile must be writable");

    let installed = installed_bundles(&rig.home).expect("the profile should list its bundles");
    let ours = installed
        .iter()
        .find(|bundle| bundle.name == BUNDLE)
        .unwrap_or_else(|| panic!("{BUNDLE} should be installed; saw {installed:?}"));
    assert!(ours.active, "{BUNDLE} should be active, not merely present");
}

/// Rules 1 and 3: an existing profile is never overwritten or stripped — only
/// the one missing entry is added, and everything else stays byte for byte.
#[tokio::test]
async fn an_existing_profile_gains_only_the_missing_bundle_entry() {
    let rig = Rig::new();
    let dir = rig.profile();
    std::fs::create_dir_all(&dir).expect("the user profile should exist");
    let user_manifest = "{\n  \"name\": \"dsh-profile-sdk\",\n  \"private\": true,\n  \"dependencies\": {\n    \"left-pad\": \"link:../left-pad\"\n  },\n  \"dsh\": {\n    \"profile\": {\n      \"bundles\": [\n        \"@deepseek-ai/dsh-base\",\n        \"@deepseek-ai/dsh-sdk-app\",\n        \"user-bundle\"\n      ]\n    }\n  },\n  \"zeron\": { \"keep\": [\"me\"] }\n}\n";
    std::fs::write(dir.join("package.json"), user_manifest).expect("the user manifest writes");
    let user_patch = "- id: keep-me\n  disabled: true\n";
    std::fs::write(dir.join("cordis.patch.yml"), user_patch).expect("the user patch writes");
    // A user's own bundle directory must survive too.
    let theirs = dir.join("node_modules/user-bundle");
    std::fs::create_dir_all(&theirs).expect("the user bundle directory exists");
    std::fs::write(theirs.join("package.json"), "{\"name\":\"user-bundle\"}")
        .expect("the user bundle manifest writes");

    rig.prepare(Some("ordinary-chat"))
        .await
        .expect("prepare should provision around the user's profile");

    assert!(
        rig.provisioned(),
        "the missing bundle should be added; manifest: {}",
        rig.manifest()
    );
    let manifest = rig.manifest();
    assert!(
        preserved_in_order(user_manifest, &manifest),
        "provisioning rewrote an existing entry; before:\n{user_manifest}\nafter:\n{manifest}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("cordis.patch.yml")).expect("the user patch reads"),
        user_patch,
        "the user's own patch layer must not be touched"
    );
    assert_eq!(
        std::fs::read_to_string(theirs.join("package.json")).expect("the user bundle manifest reads"),
        "{\"name\":\"user-bundle\"}",
        "the user's own node_modules entry must survive"
    );
}

/// Rule 4: a profile that already lists the bundle is a no-op — the exact
/// bytes stay as they were, so a reinstall or upgrade reruns safely. The
/// entry is a link where the host has a link primitive and a materialised
/// copy where it does not; the copy case compares tree bytes rather than
/// assuming `read_link` exists on the platform this test is protecting.
#[tokio::test]
async fn a_provisioned_profile_is_a_no_op_on_the_next_run() {
    let rig = Rig::new();
    rig.prepare(None).await.expect("the first run provisions");
    let first_manifest = rig.manifest();
    let first_patch = std::fs::read_to_string(rig.profile().join("cordis.patch.yml"))
        .expect("the patch layer reads");
    let entry = rig.profile().join("node_modules").join(BUNDLE);
    let first_link = std::fs::read_link(&entry).ok();
    let first_copy = first_link.is_none().then(|| snapshot_tree(&entry));

    rig.prepare(Some("another-chat"))
        .await
        .expect("the second run should be a no-op");

    assert_eq!(
        rig.manifest(),
        first_manifest,
        "a provisioned profile must not be rewritten"
    );
    assert_eq!(
        std::fs::read_to_string(rig.profile().join("cordis.patch.yml"))
            .expect("the patch layer still reads"),
        first_patch,
        "the patch layer must not be rewritten"
    );
    if let Some(first_link) = first_link {
        assert_eq!(
            std::fs::read_link(&entry).expect("the bundle link still reads"),
            first_link,
            "the bundle link must not be re-pointed"
        );
    } else {
        assert_eq!(
            snapshot_tree(&entry),
            first_copy.expect("a materialised copy has a first snapshot"),
            "a materialised copy and its marker must stay byte-identical"
        );
    }
}

/// Rule 5: a manifest that cannot be parsed is never edited — the run is
/// refused **with a reason and instructions** rather than starting with no
/// role section and no Drive tools, and the file is left exactly as it was.
#[tokio::test]
async fn an_unparseable_profile_is_left_alone_and_refuses_the_run() {
    let rig = Rig::new();
    let dir = rig.profile();
    std::fs::create_dir_all(&dir).expect("the user profile should exist");
    let broken = "{ \"dependencies\": { \"left-pad\": \"link:../left-pad\" ,, }\n";
    std::fs::write(dir.join("package.json"), broken).expect("the broken manifest writes");

    let refused = rig.prepare(Some("a-captain-chat")).await;
    assert!(
        refused.is_err(),
        "a chat run must not start with an unprovisioned profile; got {refused:?}"
    );
    let message = refused.unwrap_err();
    assert!(
        message.contains("profiles") || message.contains("package.json"),
        "the refusal should name the profile it could not read; saw {message:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("package.json")).expect("the broken manifest reads"),
        broken,
        "an unparseable profile must not be edited"
    );

    // An ordinary chat still prepares: the profile's problem is reported, not
    // turned into a wall in front of every conversation.
    rig.prepare(None)
        .await
        .expect("an ordinary chat should still prepare");
}

/// r2 defect 1 (red on `8625bbb82`): a `{}` manifest — a real file dsh itself
/// would accept — must be completed into **valid JSON**. Inserting both root
/// members at the same offset produced `{"dependencies": …"dsh": …}` with no
/// comma: the write returned Ok, and the next prepare could not parse the file
/// it had just written, refusing every chat from then on.
#[tokio::test]
async fn an_empty_object_manifest_is_completed_into_valid_json() {
    let rig = Rig::new();
    let dir = rig.profile();
    std::fs::create_dir_all(&dir).expect("the profile directory exists");
    std::fs::write(dir.join("package.json"), "{}").expect("the empty manifest writes");

    rig.prepare(Some("a-chat"))
        .await
        .expect("prepare should complete an empty manifest, not fail on it");

    let text = rig.manifest();
    let manifest: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("provisioning wrote invalid JSON ({error}): {text}"));
    assert_eq!(
        manifest["dependencies"][BUNDLE].as_str().is_some_and(|spec| spec.starts_with("link:")),
        true,
        "the completed manifest should carry the bundle dependency; saw {text}"
    );
    assert_eq!(
        manifest["dsh"]["profile"]["bundles"]
            .as_array()
            .is_some_and(|bundles| bundles.iter().any(|bundle| bundle == BUNDLE)),
        true,
        "the completed manifest should list the bundle; saw {text}"
    );
    assert!(rig.provisioned(), "the completed profile should resolve the bundle");

    let first = rig.manifest();
    rig.prepare(Some("another-chat"))
        .await
        .expect("a completed manifest is a no-op on the next prepare");
    assert_eq!(rig.manifest(), first, "the second prepare must not rewrite it");
}

/// r2 defect 2 (red on `8625bbb82`), first half: a real directory at
/// `node_modules/zeron-dsh-captains` that *is* the bundle — what a hoisted
/// pnpm install leaves — is the bundle already present. Removing it to make
/// room for a link deletes a user's installed plugin.
#[tokio::test]
async fn a_real_bundle_directory_is_left_where_it_is() {
    let rig = Rig::new();
    let entry = rig.profile().join("node_modules").join(BUNDLE);
    std::fs::create_dir_all(&entry).expect("the installed bundle directory exists");
    for name in ["package.json", "index.js"] {
        std::fs::copy(bundle_source().join(name), entry.join(name))
            .expect("the installed bundle carries its files");
    }
    let marker = entry.join("installed-by-the-user");
    std::fs::write(&marker, b"keep me").expect("the marker writes");
    let manifest_before = std::fs::read(entry.join("package.json")).expect("the bundle manifest reads");

    rig.prepare(Some("a-chat"))
        .await
        .expect("an installed bundle satisfies provisioning");

    assert!(marker.is_file(), "the installed bundle directory must not be removed");
    assert_eq!(
        std::fs::read(entry.join("package.json")).expect("the bundle manifest still reads"),
        manifest_before,
        "an installed bundle's files must be left byte-identical"
    );
    assert!(rig.provisioned(), "the installed bundle is the present bundle");

    let first = rig.manifest();
    rig.prepare(Some("another-chat"))
        .await
        .expect("the second prepare is a no-op");
    assert_eq!(rig.manifest(), first, "the second prepare must not rewrite the manifest");
    assert!(marker.is_file(), "the second prepare must not touch the directory either");
}

/// r2 defect 2 (red on `8625bbb82`), second half: a **foreign** directory at
/// the bundle's `node_modules` name is somebody else's package. It is never
/// removed; the Captain cannot get its bundle, so the run refuses with a
/// reason that names the path and says what to do — a silent skip is the exact
/// failure this issue exists for.
#[tokio::test]
async fn a_foreign_directory_is_never_removed_and_refuses_the_run() {
    let rig = Rig::new();
    let entry = rig.profile().join("node_modules").join(BUNDLE);
    std::fs::create_dir_all(&entry).expect("the foreign package directory exists");
    let foreign = "{\"name\":\"some-other-plugin\",\"version\":\"1.0.0\"}";
    std::fs::write(entry.join("package.json"), foreign).expect("the foreign manifest writes");
    let marker = entry.join("marker");
    std::fs::write(&marker, b"do not delete").expect("the marker writes");

    let refused = rig.prepare(Some("a-chat")).await;
    assert!(
        refused.is_err(),
        "a chat run must not start against a profile it could not provision; got {refused:?}"
    );
    let message = refused.unwrap_err();
    assert!(
        message.contains(BUNDLE),
        "the refusal should name the entry it could not take; saw {message:?}"
    );
    assert!(
        message.contains("delete") || message.contains("fix") || message.contains("remove"),
        "the refusal should say what to do about it; saw {message:?}"
    );
    assert!(marker.is_file(), "a foreign package directory must never be removed");
    assert_eq!(
        std::fs::read_to_string(entry.join("package.json")).expect("the foreign manifest still reads"),
        foreign,
        "a foreign package's files must be left byte-identical"
    );

    // A chatless run (title generation boots sdk-minimal) still prepares: the
    // problem is reported, not turned into a wall in front of everything.
    rig.prepare(None).await.expect("a chatless run still prepares");
}

/// Regression guard for the failure the issue was filed on: a dangling bundle
/// link left by a copied `DSH_HOME` is re-pointed at the packaged bundle, so
/// the runtime does not skip the bundle and run with zero `apply` calls.
/// Unix-only: it stages a raw symlink (the link primitive Windows lacks).
#[cfg(unix)]
#[tokio::test]
async fn a_dangling_bundle_link_is_repointed() {
    let rig = Rig::new();
    let dir = rig.profile();
    std::fs::create_dir_all(dir.join("node_modules")).expect("the profile node_modules exists");
    std::fs::write(
        dir.join("package.json"),
        format!(
            "{{\n  \"name\": \"dsh-profile-sdk\",\n  \"dependencies\": {{ \"{BUNDLE}\": \"link:/no/such/bundle\" }},\n  \"dsh\": {{ \"profile\": {{ \"bundles\": [\"{BUNDLE}\"] }} }}\n}}\n"
        ),
    )
    .expect("the copied manifest writes");
    std::os::unix::fs::symlink("/no/such/bundle", dir.join("node_modules").join(BUNDLE))
        .expect("the dangling link writes");

    rig.prepare(Some("a-chat"))
        .await
        .expect("a dangling link must be re-pointed, not refused");

    assert!(
        rig.resolves_bundle(),
        "the copied profile's dangling link should resolve the packaged bundle"
    );
}
