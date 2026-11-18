//! The dsh bundles installed in Zeron's `sdk` profile, and the command that
//! installs or removes one.
//!
//! The profile is a plain pnpm project (`profiles/sdk/package.json` plus its
//! `node_modules`), so listing what is installed is a read of two manifests per
//! bundle and nothing else: no pnpm, no dsh, no writes. Changing it is dsh's
//! own `plugin` subcommand, built here and run by the caller.

use std::{
    ffi::OsString,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::{
    HarnessError,
    process::{Command, Stdio},
};

/// Bundles that ship inside dsh itself; the profile lists them as
/// dependencies but they are not something the user installed.
const IN_BOX: &str = "@deepseek-ai/dsh-base";

/// One dsh bundle installed in the `sdk` profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBundle {
    pub name: String,
    pub spec: String,
    /// Listed in the profile's `dsh.profile.bundles`, so dsh loads it.
    pub active: bool,
    /// Some part of it renders in a browser, which zgx cannot host.
    pub needs_client: bool,
    pub version: Option<String>,
    pub engines_dsh: Option<String>,
    pub description: Option<String>,
}

/// The `sdk` profile's directory inside a harness home.
pub fn profile_dir(home: &Path) -> PathBuf {
    home.join("profiles").join("sdk")
}

fn read_json(path: &Path) -> Result<Option<Value>, HarnessError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| HarnessError::Protocol(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(HarnessError::Io(e)),
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The bundles installed in the `sdk` profile, sorted by name.
///
/// A home with no profile has nothing installed, which is not an error.
pub fn installed_bundles(home: &Path) -> Result<Vec<InstalledBundle>, HarnessError> {
    let dir = profile_dir(home);
    let Some(profile) = read_json(&dir.join("package.json"))? else {
        return Ok(Vec::new());
    };
    let active: Vec<&str> = profile["dsh"]["profile"]["bundles"]
        .as_array()
        .map(|bundles| bundles.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(dependencies) = profile["dependencies"].as_object() else {
        return Ok(Vec::new());
    };
    let mut bundles: Vec<InstalledBundle> = dependencies
        .iter()
        .filter(|(name, _)| !name.starts_with(IN_BOX))
        .map(|(name, spec)| {
            // A bundle whose own manifest is missing or unreadable still
            // installed; only its metadata is unknown.
            let manifest = read_json(&dir.join("node_modules").join(name).join("package.json"))
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            InstalledBundle {
                name: name.clone(),
                spec: spec.as_str().unwrap_or_default().to_owned(),
                active: active.contains(&name.as_str()),
                needs_client: needs_client(&dir, name, &manifest),
                version: text(&manifest, "version"),
                engines_dsh: text(&manifest["engines"], "dsh"),
                description: text(&manifest, "description"),
            }
        })
        .collect();
    bundles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(bundles)
}

/// Whether some part of the bundle renders in a browser: its own manifest
/// declares `dsh.client`, or a package its patch file inserts does. Missing
/// or unreadable files read as no — this describes what is installed, so it
/// can never fail.
fn needs_client(dir: &Path, name: &str, manifest: &Value) -> bool {
    if manifest["dsh"]["client"].is_object() {
        return true;
    }
    let Some(patch) = manifest["dsh"]["bundle"]["patch"].as_str() else {
        return false;
    };
    let Ok(patch) = std::fs::read_to_string(dir.join("node_modules").join(name).join(patch))
    else {
        return false;
    };
    patch_rows(&patch).any(|row| {
        row != name
            && read_json(&dir.join("node_modules").join(row).join("package.json"))
                .ok()
                .flatten()
                .is_some_and(|manifest| manifest["dsh"]["client"].is_object())
    })
}

/// The package names a patch file's rows name. A patch is YAML, but the
/// rows that matter here are flat `name:` entries, so a line scan — not a
/// YAML dependency — is enough: a trimmed `name:` or `- name:` line
/// contributes its value, quotes stripped.
fn patch_rows(patch: &str) -> impl Iterator<Item = &str> {
    patch.lines().filter_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix('-').map(str::trim).unwrap_or(line);
        line.strip_prefix("name:")
            .map(|name| name.trim().trim_matches(['\'', '"']))
    })
}

/// What to do to the `sdk` profile: install a bundle, or remove one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginOp {
    Add(String),
    Remove(String),
}

/// Longest spec accepted: anything past this is a mistake, not a bundle.
const MAX_SPEC: usize = 512;

/// Whether `spec` is something `dsh plugin add` can install: an npm name with
/// an optional `@version`, a `github:<owner>/<repo>[#<ref>]` shorthand, a
/// `git+https://` URL, or an absolute path (a directory, or a packed `.tgz`).
///
/// A spec reaches a child process's argv, so anything that could read as an
/// option (`-rf`), split into two arguments (whitespace), or resolve against
/// the working directory (`./x`) is refused.
pub fn valid_spec(spec: &str) -> bool {
    if spec.is_empty() || spec.len() > MAX_SPEC || spec.starts_with('-') {
        return false;
    }
    if spec.chars().any(char::is_whitespace) {
        return false;
    }
    npm_spec(spec)
        || github_spec(spec)
        || spec
            .strip_prefix("git+https://")
            .is_some_and(|rest| !rest.is_empty())
        || Path::new(spec).has_root()
}

/// `^(@[a-z0-9-~][a-z0-9-._~]*/)?[a-z0-9-~][a-z0-9-._~]*(@[^\s]+)?$`, by hand:
/// the crate has no regex dependency.
fn npm_spec(spec: &str) -> bool {
    let rest = match spec.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, rest)) if name_start(scope) && name_rest(scope) => rest,
            _ => return false,
        },
        None => spec,
    };
    let (name, version) = match rest.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (rest, None),
    };
    name_start(name)
        && name_rest(name)
        && version.is_none_or(|v| !v.is_empty() && !v.chars().any(char::is_whitespace))
}

fn name_start(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '~'))
}

fn name_rest(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_' | '~'))
}

/// `github:<owner>/<repo>[#<ref>]`.
fn github_spec(spec: &str) -> bool {
    let Some(rest) = spec.strip_prefix("github:") else {
        return false;
    };
    let (repo, reference) = match rest.split_once('#') {
        Some((repo, reference)) => (repo, Some(reference)),
        None => (rest, None),
    };
    let Some((owner, repo)) = repo.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !repo.is_empty()
        && !repo.contains('/')
        && reference.is_none_or(|r| !r.is_empty())
}

/// The pinned dsh runtime entry inside the SDK root, the same file
/// `sdk_installed` probes.
fn runtime_entry(sdk_root: &Path) -> PathBuf {
    sdk_root.join("node_modules/@deepseek-ai/dsh/lib/bin.js")
}

/// The `dsh plugin --profile sdk add|remove <spec>` child, built but not run.
///
/// `node` runs the pinned runtime entry inside `sdk_root`; the child works in
/// `home` with `DSH_HOME` pointing at it, so dsh resolves the same profile
/// [`installed_bundles`] reads. An invalid spec is refused for both operations
/// before any process exists.
pub fn plugin_command(
    node: &Path,
    sdk_root: &Path,
    home: &Path,
    op: &PluginOp,
) -> Result<Command, HarnessError> {
    let (verb, spec) = match op {
        PluginOp::Add(spec) => ("add", spec),
        PluginOp::Remove(spec) => ("remove", spec),
    };
    if !valid_spec(spec) {
        return Err(HarnessError::Protocol(format!(
            "invalid plugin spec: {spec}"
        )));
    }
    let mut command = Command::new(node);
    command
        .arg(runtime_entry(sdk_root))
        .arg("plugin")
        .arg("--profile")
        .arg("sdk")
        .arg(verb)
        .arg(spec)
        .current_dir(home)
        .env("DSH_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::compose_child_path(&mut command, node);
    Ok(command)
}

/// How long `dsh plugin` may take: pnpm resolves, downloads and links a
/// bundle, which is slow on a cold store but never unbounded.
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(600);

/// Longest log tail kept for the UI.
const LOG_TAIL: usize = 4000;

/// The outcome of one `dsh plugin` run, small enough to hand to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRun {
    pub ok: bool,
    pub exit_code: Option<i32>,
    /// The end of the child's output, redacted, for the failure message.
    pub log_tail: String,
}

/// Whether `pnpm` is on PATH. `dsh plugin` shells out to it, so a host
/// without it cannot manage plugins at all.
pub fn pnpm_available() -> bool {
    pnpm_available_with(&|name| std::env::var_os(name), crate::shell_env::user_path())
}

fn pnpm_available_with(
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
) -> bool {
    crate::executable::find_on_paths_with(
        "pnpm",
        Vec::new(),
        env,
        login_shell_path,
        crate::executable::Platform::current(),
    )
    .is_some()
}

/// Run `op` against the `sdk` profile and report how it went.
///
/// pnpm is checked first: `dsh plugin` needs it, and a host without it gets
/// one clear message instead of a child that fails on its own.
pub async fn run_plugin_op(
    sdk_root: &Path,
    home: &Path,
    op: PluginOp,
) -> Result<PluginRun, HarnessError> {
    run_plugin_op_with(pnpm_available(), super::node(), sdk_root, home, op).await
}

async fn run_plugin_op_with(
    pnpm: bool,
    node: Option<PathBuf>,
    sdk_root: &Path,
    home: &Path,
    op: PluginOp,
) -> Result<PluginRun, HarnessError> {
    if !pnpm {
        return Err(HarnessError::NotInstalled(
            "Install pnpm 10 or newer to manage DeepSeek plugins".into(),
        ));
    }
    let node = node.ok_or_else(|| {
        HarnessError::NotInstalled("Install Node.js to manage DeepSeek plugins".into())
    })?;
    let mut child = plugin_command(&node, sdk_root, home, &op)?.spawn()?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    // The child stays owned here so a timeout can still kill it.
    let running = &mut child;
    let collected = tokio::time::timeout(PLUGIN_TIMEOUT, async move {
        let (out, err) = tokio::join!(read_all(stdout), read_all(stderr));
        (out, err, running.wait().await)
    })
    .await;
    match collected {
        Ok((out, err, status)) => {
            let status = status?;
            Ok(PluginRun {
                ok: status.success(),
                exit_code: status.code(),
                log_tail: tail(&output_text(&out, &err)),
            })
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Ok(PluginRun {
                ok: false,
                exit_code: None,
                log_tail: tail(&format!(
                    "dsh plugin timed out after {} seconds",
                    PLUGIN_TIMEOUT.as_secs()
                )),
            })
        }
    }
}

/// Everything a child pipe wrote, or nothing when it was not captured.
async fn read_all<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes).await;
    }
    bytes
}

/// stdout then stderr, lossy: what the child printed, in order.
fn output_text(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(stderr));
    text
}

/// The last [`LOG_TAIL`] chars of `log`, cut on a char boundary and redacted.
fn tail(log: &str) -> String {
    let start = log
        .char_indices()
        .rev()
        .nth(LOG_TAIL - 1)
        .map_or(0, |(at, _)| at);
    crate::redact_secrets(&log[start..])
}

/// The CLI's compatibility refusal, as one line for the UI.
///
/// `dsh plugin` exits non-zero when a bundle's `engines.dsh` does not accept
/// the bundled dsh; the reason is the first log line that says so.
pub fn engines_refusal(log: &str) -> Option<String> {
    let line = log
        .lines()
        .map(str::trim)
        .find(|line| line.contains("engines.dsh") || line.contains("does not satisfy"))?;
    Some(format!(
        "This plugin is not compatible with the bundled dsh version (engines.dsh). {line}"
    ))
}

/// `package`, allowed to run its build scripts in the `sdk` profile.
///
/// pnpm 10 skips a dependency's install-time scripts until the package is
/// listed under `allowBuilds` in the profile's pnpm-workspace.yaml. The
/// file is flat keys, so the row is written by hand — no YAML dependency —
/// preserving what is already there. A package already listed keeps the
/// user's own value for it, whatever it is. A name that could not sit in a
/// key (empty, a leading `-` or `.`, a space or `:` in it) is refused
/// before anything is written.
pub fn allow_build(home: &Path, package: &str) -> Result<(), HarnessError> {
    if !valid_build_name(package) {
        return Err(HarnessError::Protocol(format!(
            "invalid allowBuilds package: {package:?}"
        )));
    }
    let path = profile_dir(home).join("pnpm-workspace.yaml");
    let yaml = match std::fs::read_to_string(&path) {
        Ok(yaml) => yaml,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let key = build_key(package);
    let out = match allow_builds(&yaml) {
        // The user's own say for it stands, whatever it is.
        Some(AllowBlock::Rows { names, .. }) | Some(AllowBlock::Inline { names, .. })
            if names.iter().any(|name| name.as_str() == package) =>
        {
            return Ok(())
        }
        // A rows block takes the new row at its last row's own indent.
        Some(AllowBlock::Rows { at, indent, .. }) => {
            let mut out = yaml;
            out.insert_str(at, &format!("{indent}{key}: true\n"));
            out
        }
        // An inline `{…}` gets the key inside its braces.
        Some(AllowBlock::Inline { brace, empty, .. }) => {
            let mut out = yaml;
            let pad = if empty { "" } else { ", " };
            out.insert_str(brace, &format!("{pad}{key}: true"));
            out
        }
        None => {
            let mut out = yaml;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("allowBuilds:\n");
            out.push_str(&format!("  {key}: true\n"));
            out
        }
    };
    std::fs::create_dir_all(profile_dir(home))?;
    std::fs::write(&path, out)?;
    Ok(())
}

/// A name as an `allowBuilds` key: `[@a-z0-9/._-]+`, so a row it writes
/// can never carry a space or a `:` into a second place, and nothing that
/// is not a package's name (a leading `-` or `.`) starts it.
fn valid_build_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '@' | '/' | '.' | '_' | '-')
        })
}

/// `name` as a YAML key, quoted when a plain scalar would not stay one: a
/// scope's leading `@` is a reserved character, and a bare `true`, `123`
/// or `1.5` would read as a value, not a name.
fn build_key(name: &str) -> String {
    if name.starts_with('@')
        || matches!(name, "true" | "false" | "null")
        || name.parse::<f64>().is_ok()
    {
        format!("\"{name}\"")
    } else {
        name.to_owned()
    }
}

/// What an `allowBuilds:` line heads, for [`allow_build`]: a block of
/// indented rows, or an inline `{name: value, …}` map.
enum AllowBlock {
    /// Indented rows: the names already there, the byte offset a new row
    /// lands at (just past the block's last line), and the indent rows use.
    Rows {
        names: Vec<String>,
        at: usize,
        indent: String,
    },
    /// An inline map: the names inside it, the byte offset of its closing
    /// `}`, and whether it holds nothing yet.
    Inline {
        names: Vec<String>,
        brace: usize,
        empty: bool,
    },
}

/// The `allowBuilds:` block in `yaml`, when the file has one. Only a
/// top-level key counts — one nested under another map is not pnpm's.
fn allow_builds(yaml: &str) -> Option<AllowBlock> {
    let mut at = 0;
    for line in yaml.split_inclusive('\n') {
        let text = line.trim_end();
        if let Some(rest) = text.strip_prefix("allowBuilds:") {
            let rest = rest.trim_start();
            if rest.is_empty() || rest.starts_with('#') {
                return Some(rows_block(yaml, at + line.len()));
            }
            if rest.starts_with('{') {
                return inline_block(at, text);
            }
            // A value that is not a map is a broken file's own problem:
            // treated as no block, so a fresh one is appended.
            return None;
        }
        at += line.len();
    }
    None
}

/// The rows under an `allowBuilds:` header: each line that is blank,
/// indented, or an indented comment still belongs to the block; the next
/// top-level line ends it.
fn rows_block(yaml: &str, from: usize) -> AllowBlock {
    let mut names = Vec::new();
    let mut indent = None;
    let mut insert_at = from;
    let mut at = from;
    for line in yaml[from..].split_inclusive('\n') {
        let text = line.trim_end();
        if !text.is_empty() && !text.starts_with([' ', '\t']) {
            break;
        }
        if !text.is_empty() {
            insert_at = at + line.len();
            let body = text.trim_start();
            if !body.starts_with('#') {
                if indent.is_none() {
                    indent = Some(text[..text.len() - body.len()].to_owned());
                }
                if let Some(name) = row_name(body) {
                    names.push(name);
                }
            }
        }
        at += line.len();
    }
    AllowBlock::Rows {
        names,
        at: insert_at,
        indent: indent.unwrap_or_else(|| "  ".into()),
    }
}

/// An `allowBuilds: {…}` on one line: the names inside the braces, and
/// where its `}` is. One that never closes on the line is left alone.
fn inline_block(at: usize, text: &str) -> Option<AllowBlock> {
    let open = text.find('{')?;
    let close = open + text[open..].find('}')?;
    let inner = &text[open + 1..close];
    Some(AllowBlock::Inline {
        names: inner.split(',').filter_map(row_name).collect(),
        brace: at + close,
        empty: inner.trim().is_empty(),
    })
}

/// The name a row or inline entry keys on — `esbuild` from
/// `esbuild: true`, `@scope/p` from `"@scope/p": false` — quotes dropped.
fn row_name(text: &str) -> Option<String> {
    let text = text.trim_start();
    let name = match text.chars().next()? {
        quote @ ('\'' | '"') => {
            let end = text[1..].find(quote)? + 1;
            &text[1..end]
        }
        _ => text.split([':', ' ']).next()?,
    };
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn profile(home: &Path, contents: &str) {
        write(&profile_dir(home).join("package.json"), contents);
    }

    fn bundle(home: &Path, name: &str, contents: &str) {
        write(
            &profile_dir(home)
                .join("node_modules")
                .join(name)
                .join("package.json"),
            contents,
        );
    }

    fn patch(home: &Path, name: &str, contents: &str) {
        write(
            &profile_dir(home)
                .join("node_modules")
                .join(name)
                .join("cordis.patch.yml"),
            contents,
        );
    }

    #[test]
    fn a_home_without_a_profile_has_no_bundles() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(installed_bundles(dir.path()).unwrap(), Vec::new());
    }

    #[test]
    fn an_active_bundle_reports_its_own_manifest() {
        let dir = tempfile::tempdir().unwrap();
        profile(
            dir.path(),
            r#"{"name":"dsh-profile-sdk","private":true,
                "dependencies":{"dsh-hello-plugin":"file:/abs/path",
                                "@deepseek-ai/dsh-base-core":"0.2.0"},
                "dsh":{"profile":{"bundles":["dsh-hello-plugin"]}}}"#,
        );
        bundle(
            dir.path(),
            "dsh-hello-plugin",
            r#"{"name":"dsh-hello-plugin","version":"1.2.3",
                "description":"Says hello","engines":{"dsh":">=0.2"}}"#,
        );
        let bundles = installed_bundles(dir.path()).unwrap();
        // The in-box base package is not something the user installed.
        assert_eq!(bundles.len(), 1);
        assert_eq!(
            bundles[0],
            InstalledBundle {
                name: "dsh-hello-plugin".into(),
                spec: "file:/abs/path".into(),
                active: true,
                needs_client: false,
                version: Some("1.2.3".into()),
                engines_dsh: Some(">=0.2".into()),
                description: Some("Says hello".into()),
            }
        );
    }

    #[test]
    fn a_dependency_the_profile_does_not_load_is_inactive() {
        let dir = tempfile::tempdir().unwrap();
        profile(
            dir.path(),
            r#"{"dependencies":{"dsh-hello-plugin":"file:/abs/path","dsh-quiet":"^2.0.0"},
                "dsh":{"profile":{"bundles":["dsh-hello-plugin"]}}}"#,
        );
        // A bundle with no manifest of its own still lists, without metadata.
        let bundles = installed_bundles(dir.path()).unwrap();
        let names: Vec<&str> = bundles.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["dsh-hello-plugin", "dsh-quiet"]);
        assert!(bundles[0].active);
        assert_eq!(bundles[1].spec, "^2.0.0");
        assert!(!bundles[1].active);
        assert!(!bundles[1].needs_client);
        assert_eq!(bundles[1].version, None);
        assert_eq!(bundles[1].engines_dsh, None);
        assert_eq!(bundles[1].description, None);
    }

    #[test]
    fn a_manifest_with_dsh_client_needs_a_client() {
        let dir = tempfile::tempdir().unwrap();
        profile(
            dir.path(),
            r#"{"dependencies":{"dsh-voice":"^1.0","dsh-plain":"^1.0"},
                "dsh":{"profile":{"bundles":["dsh-voice","dsh-plain"]}}}"#,
        );
        bundle(
            dir.path(),
            "dsh-voice",
            r#"{"name":"dsh-voice","dsh":{"client":{"platform":"web"}}}"#,
        );
        // `dsh.client` that is not an object does not count.
        bundle(
            dir.path(),
            "dsh-plain",
            r#"{"name":"dsh-plain","dsh":{"client":true}}"#,
        );
        let bundles = installed_bundles(dir.path()).unwrap();
        assert!(!bundles[0].needs_client);
        assert!(bundles[1].needs_client);
    }

    #[test]
    fn a_patch_row_with_dsh_client_needs_a_client() {
        let dir = tempfile::tempdir().unwrap();
        profile(
            dir.path(),
            r#"{"dependencies":{"dsh-voice-bundle":"^1.0"},
                "dsh":{"profile":{"bundles":["dsh-voice-bundle"]}}}"#,
        );
        bundle(
            dir.path(),
            "dsh-voice-bundle",
            r#"{"name":"dsh-voice-bundle",
                "dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        );
        patch(
            dir.path(),
            "dsh-voice-bundle",
            "- insert:\n    - id: ui\n      name: '@scope/dsh-voice-ui'\n",
        );
        bundle(
            dir.path(),
            "@scope/dsh-voice-ui",
            r#"{"name":"@scope/dsh-voice-ui","dsh":{"client":{"platform":"web"}}}"#,
        );
        let bundles = installed_bundles(dir.path()).unwrap();
        assert!(bundles[0].needs_client);
    }

    #[test]
    fn a_bundle_that_patches_itself_in_needs_no_client() {
        let dir = tempfile::tempdir().unwrap();
        profile(
            dir.path(),
            r#"{"dependencies":{"dsh-hello-plugin":"file:/abs/path"},
                "dsh":{"profile":{"bundles":["dsh-hello-plugin"]}}}"#,
        );
        bundle(
            dir.path(),
            "dsh-hello-plugin",
            r#"{"name":"dsh-hello-plugin",
                "dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        );
        patch(
            dir.path(),
            "dsh-hello-plugin",
            "- insert:\n    - id: hello\n      name: 'dsh-hello-plugin'\n",
        );
        let bundles = installed_bundles(dir.path()).unwrap();
        assert!(!bundles[0].needs_client);
    }

    #[test]
    fn a_missing_patch_file_needs_no_client() {
        let dir = tempfile::tempdir().unwrap();
        profile(
            dir.path(),
            r#"{"dependencies":{"dsh-hello-plugin":"file:/abs/path"},
                "dsh":{"profile":{"bundles":["dsh-hello-plugin"]}}}"#,
        );
        bundle(
            dir.path(),
            "dsh-hello-plugin",
            r#"{"name":"dsh-hello-plugin",
                "dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        );
        let bundles = installed_bundles(dir.path()).unwrap();
        assert!(!bundles[0].needs_client);
    }

    #[test]
    fn a_corrupt_profile_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        profile(dir.path(), "{not json");
        assert!(matches!(
            installed_bundles(dir.path()),
            Err(HarnessError::Protocol(_))
        ));
    }

    /// The command for `op` against fixed paths, with the home it was built
    /// for. Built, never spawned.
    fn command(op: &PluginOp) -> (tempfile::TempDir, Command) {
        let home = tempfile::tempdir().unwrap();
        let command = plugin_command(
            Path::new("/usr/bin/node"),
            Path::new("/sdk"),
            home.path(),
            op,
        )
        .unwrap();
        (home, command)
    }

    fn args(command: &Command) -> Vec<&OsStr> {
        command.as_std().get_args().collect()
    }

    #[test]
    fn add_and_remove_build_the_dsh_plugin_command() {
        let entry = runtime_entry(Path::new("/sdk"));
        for (op, verb, spec) in [
            (
                PluginOp::Add("dsh-hello-plugin".into()),
                "add",
                "dsh-hello-plugin",
            ),
            (
                PluginOp::Remove("@scope/pkg@1.2.3".into()),
                "remove",
                "@scope/pkg@1.2.3",
            ),
        ] {
            let (home, command) = command(&op);
            let std = command.as_std();
            assert_eq!(std.get_program(), OsStr::new("/usr/bin/node"));
            assert_eq!(
                args(&command),
                [
                    entry.as_os_str(),
                    OsStr::new("plugin"),
                    OsStr::new("--profile"),
                    OsStr::new("sdk"),
                    OsStr::new(verb),
                    OsStr::new(spec),
                ]
            );
            assert_eq!(std.get_current_dir(), Some(home.path()));
            let dsh_home = std
                .get_envs()
                .find(|(key, _)| *key == OsStr::new("DSH_HOME"))
                .and_then(|(_, value)| value);
            assert_eq!(dsh_home, Some(home.path().as_os_str()));
        }
    }

    #[test]
    fn invalid_specs_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let long = "a".repeat(MAX_SPEC + 1);
        for spec in ["-rf", "a b", "./x", "", long.as_str()] {
            assert!(!valid_spec(spec), "{spec:?} should be invalid");
            for op in [PluginOp::Add(spec.into()), PluginOp::Remove(spec.into())] {
                assert!(
                    matches!(
                        plugin_command(
                            Path::new("/usr/bin/node"),
                            Path::new("/sdk"),
                            home.path(),
                            &op
                        ),
                        Err(HarnessError::Protocol(_))
                    ),
                    "{op:?} should be refused"
                );
            }
        }
    }

    #[test]
    fn valid_specs_are_accepted() {
        let home = tempfile::tempdir().unwrap();
        for spec in [
            "dsh-hello-plugin",
            "@scope/pkg@1.2.3",
            "github:owner/repo#main",
            "git+https://github.com/o/r.git",
            "/abs/path",
            "/abs/bundle.tgz",
        ] {
            assert!(valid_spec(spec), "{spec:?} should be valid");
            assert!(
                plugin_command(
                    Path::new("/usr/bin/node"),
                    Path::new("/sdk"),
                    home.path(),
                    &PluginOp::Add(spec.into())
                )
                .is_ok(),
                "{spec:?} should build a command"
            );
        }
    }

    fn add() -> PluginOp {
        PluginOp::Add("dsh-hello-plugin".into())
    }

    #[test]
    fn pnpm_is_missing_without_a_path() {
        assert!(!pnpm_available_with(&|_: &str| None, None));
    }

    #[tokio::test]
    async fn a_host_without_pnpm_is_told_to_install_it() {
        let dir = tempfile::tempdir().unwrap();
        let node = Some(PathBuf::from("/usr/bin/node"));
        let error = run_plugin_op_with(false, node, Path::new("/sdk"), dir.path(), add())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, HarnessError::NotInstalled(message) if message.contains("pnpm")),
            "{error:?}"
        );
        // pnpm is checked first: a host with neither hears about pnpm.
        let error = run_plugin_op_with(false, None, Path::new("/sdk"), dir.path(), add())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, HarnessError::NotInstalled(message) if message.contains("pnpm")),
            "{error:?}"
        );
    }

    #[test]
    fn the_log_tail_keeps_the_last_four_thousand_chars() {
        let log = "é".repeat(LOG_TAIL + 500);
        let kept = tail(&log);
        assert_eq!(kept.chars().count(), LOG_TAIL);
        assert!(log.ends_with(&kept));
        assert_eq!(tail("added 1 package"), "added 1 package");
        // Credentials never reach the UI.
        assert_eq!(tail("Bearer secret"), "Bearer [REDACTED]");
    }

    #[test]
    fn an_engines_refusal_is_explained_in_one_line() {
        for log in [
            "pnpm install failed\n  dsh-hello-plugin requires engines.dsh >=0.3\n",
            "ERR_PNPM: dsh-hello-plugin@1.0.0 does not satisfy engines.dsh\n",
        ] {
            let refusal = engines_refusal(log).unwrap();
            assert!(refusal.starts_with("This plugin is not compatible"));
            assert!(refusal.contains("dsh-hello-plugin"));
            assert!(!refusal.contains('\n'), "{refusal:?}");
        }
        assert_eq!(engines_refusal("added 1 package in 2s"), None);
    }

    fn workspace(home: &Path) -> PathBuf {
        profile_dir(home).join("pnpm-workspace.yaml")
    }

    fn read(home: &Path) -> String {
        std::fs::read_to_string(workspace(home)).unwrap()
    }

    #[test]
    fn allow_build_writes_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        allow_build(dir.path(), "dsh-hello-plugin").unwrap();
        assert_eq!(
            read(dir.path()),
            "allowBuilds:\n  dsh-hello-plugin: true\n"
        );
    }

    #[test]
    fn allow_build_appends_to_a_file_with_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        write(&workspace(dir.path()), "packages:\n  - '.'\ncatalogs: {}\n");
        allow_build(dir.path(), "dsh-hello-plugin").unwrap();
        assert_eq!(
            read(dir.path()),
            "packages:\n  - '.'\ncatalogs: {}\nallowBuilds:\n  dsh-hello-plugin: true\n"
        );
    }

    #[test]
    fn allow_build_joins_an_existing_block() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &workspace(dir.path()),
            "allowBuilds:\n  esbuild: true\n  # kept as found\nother: 1\n",
        );
        allow_build(dir.path(), "dsh-hello-plugin").unwrap();
        // The row lands inside the block, before the next top-level key.
        assert_eq!(
            read(dir.path()),
            "allowBuilds:\n  esbuild: true\n  # kept as found\n  dsh-hello-plugin: true\nother: 1\n"
        );
    }

    #[test]
    fn allow_build_never_rewrites_a_name_already_there() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &workspace(dir.path()),
            "allowBuilds:\n  esbuild: true\n  \"@scope/dsh-denied\": false\n",
        );
        allow_build(dir.path(), "dsh-hello-plugin").unwrap();
        // A scoped name is quoted: a `@` cannot open a plain YAML scalar.
        allow_build(dir.path(), "@scope/dsh-plugin").unwrap();
        let before = read(dir.path());
        assert_eq!(
            before,
            "allowBuilds:\n  esbuild: true\n  \"@scope/dsh-denied\": false\n  dsh-hello-plugin: true\n  \"@scope/dsh-plugin\": true\n"
        );
        // Idempotent: the same ask again changes nothing.
        allow_build(dir.path(), "dsh-hello-plugin").unwrap();
        allow_build(dir.path(), "@scope/dsh-plugin").unwrap();
        assert_eq!(read(dir.path()), before);
        // And a user's own `false` for it is never overwritten.
        allow_build(dir.path(), "@scope/dsh-denied").unwrap();
        assert_eq!(read(dir.path()), before);
    }

    #[test]
    fn a_bad_name_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["", "-pkg", ".pkg", "a b", "a:b", "UPPER", "pkg\nother: 1"] {
            assert!(
                matches!(
                    allow_build(dir.path(), bad),
                    Err(HarnessError::Protocol(_))
                ),
                "{bad:?} should be refused"
            );
        }
        assert!(!workspace(dir.path()).exists());
    }
}
