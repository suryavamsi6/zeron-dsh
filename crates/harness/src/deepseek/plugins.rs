//! The dsh bundles installed in Zeron's `sdk` profile, and the command that
//! installs or removes one.
//!
//! The profile is a plain pnpm project (`profiles/sdk/package.json` plus its
//! `node_modules`), so listing what is installed is a read of two manifests per
//! bundle and nothing else: no pnpm, no dsh, no writes. Changing it is dsh's
//! own `plugin` subcommand, built here and run by the caller.
//!
//! YAML here is intentionally a checked subset, not a general YAML editor.
//! The files dsh writes are small and machine-shaped: a top-level sequence of
//! patch rows, simple block mappings, single-line JSON flow rows, and the flat
//! `allowBuilds` map. The line parser keeps byte positions so an edit preserves
//! comments, quoting and unrelated fields exactly; a YAML object tree would
//! lose those positions, and tree-sitter would add the syntax crate and a
//! query harness without supplying the patch-row semantics. Anchors, aliases,
//! multiline flow collections, nested row structure and scalar encodings the
//! line read cannot verify are outside the subset when they carry a row's
//! identity or structure. They are not rewritten: the row is uneditable or the
//! write is refused untouched. `yaml-rust2` is used only as a syntax/root-shape
//! guard before editing; the dependency is already in this crate for skill
//! frontmatter, so keeping the subset costs no new supply-chain or build-time
//! weight.

use std::{
    ffi::OsString,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::Value;
use tokio::io::AsyncReadExt;
use yaml_rust2::parser::{Event, EventReceiver, Parser};

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

/// One plugin entry a bundle's patch rows at, for the page's per-row
/// switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleRow {
    /// The `id` the row answers to: what a `cordis.patch.yml` override
    /// names, so what a switch writes.
    pub id: String,
    /// A `title:`/`name:` the row carries, for a label under the id.
    pub title: Option<String>,
    /// Whether the row is off as it stands: the profile's override when
    /// it sets one, else the row's own `disabled:` (absent is on).
    pub disabled: bool,
    /// The profile's `cordis.patch.yml` sets this row's `disabled`.
    pub overridden: bool,
    /// It can be switched from here: its `id` is a plain string that
    /// writes back cleanly, and nothing in the row is `!!js`, which a
    /// line read cannot safely answer for.
    pub editable: bool,
    /// Why it can't be switched, when it can't.
    pub note: Option<String>,
}

/// The fields of one patch row that a line read answers: its `id`, a
/// `title:`/`name:` for a label, the `disabled` it carries, and whether
/// `!!js` anywhere in its text makes it unsafe to read or write by line.
#[derive(Debug, Default)]
struct PatchRow {
    id: Option<String>,
    title: Option<String>,
    disabled: Option<bool>,
    dynamic: bool,
}

/// Leading whitespace of a line, its indent in columns.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Whether `text` holds a `!!` tag — `!!js` being the one these files
/// carry: after a `:` (a key's value), a `-` (a list item), a flow
/// opening, or at the line's start. One inside a quoted string would
/// read as a tag too; a row marked dynamic that isn't only ever shows a
/// switch it can't use, never one that writes wrongly.
fn has_tag(text: &str) -> bool {
    text.match_indices("!!").any(|(at, _)| {
        at == 0 || matches!(text.as_bytes()[at - 1], b':' | b'-' | b' ' | b'\t' | b'[' | b'{' | b',')
    })
}

/// A scalar as a YAML line carries it: `'…'` (its `''` an escaped `'`)
/// and `"…"` unquoted, a plain one trimmed of a ` #…` tail. A value that
/// is not a scalar — a `{…}`/`[…]`, a `!!` tag, an anchor — is None, not
/// a guess.
fn scalar(text: &str) -> Option<String> {
    let text = text.trim();
    match text.chars().next()? {
        '\'' => {
            let mut out = String::new();
            let mut rest = &text[1..];
            loop {
                let end = rest.find('\'')?;
                out.push_str(&rest[..end]);
                rest = &rest[end + 1..];
                // `''` inside is one `'`, not the end.
                if rest.starts_with('\'') {
                    out.push('\'');
                    rest = &rest[1..];
                } else {
                    return Some(out);
                }
            }
        }
        '"' => {
            let mut escaped = false;
            for (at, c) in text[1..].char_indices() {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    return serde_json::from_str::<String>(&text[..at + 2])
                        .ok()
                        .or_else(|| Some(text[1..at + 1].to_owned()));
                }
            }
            None
        }
        '!' | '&' | '*' | '[' | '{' | '|' | '>' | '%' | '@' | '`' => None,
        _ => {
            let plain = text.split(" #").next()?.trim_end();
            (!plain.is_empty() && !plain.starts_with(['[', '{'])).then(|| plain.to_owned())
        }
    }
}

/// A `disabled:` scalar as a bool, `true`/`false` being all dsh writes;
/// anything else (a `!!js`, a `yes`) is not a switch's answer.
fn bool_scalar(text: &str) -> Option<bool> {
    match scalar(text)?.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// One map's fields read at its field indent: `first` is its first
/// field's `key: value` text (for a `- ` row, what follows the dash),
/// `lines` the block under it. Only lines at exactly the field indent
/// are its keys — deeper lines are a key's own block, and `id:` nested
/// in a `config:` is never mistaken for the row's.
fn entry_fields(first: &str, lines: &[&str], field_indent: usize) -> PatchRow {
    let mut row = PatchRow {
        dynamic: has_tag(first) || lines.iter().any(|line| has_tag(line)),
        ..PatchRow::default()
    };
    let mut title = None;
    let mut name = None;
    let mut take = |line: &str| {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "id" => row.id = scalar(value),
                "disabled" => row.disabled = bool_scalar(value),
                "title" => title = scalar(value),
                "name" => name = scalar(value),
                _ => {}
            }
        }
    };
    take(first);
    for line in lines {
        let text = line.trim_end();
        if text.is_empty() || indent_of(text) != field_indent {
            continue;
        }
        take(text.trim_start());
    }
    row.title = title.or(name);
    row
}

/// A `{…}` or `[…]`'s inside split on the commas that sit outside quotes
/// and nested opens — flow is the form the runtime's own JSON rows take.
fn flow_parts(inner: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    for (at, c) in inner.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(inner[start..at].trim().to_owned());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(inner[start..].trim().to_owned());
    parts.retain(|part| !part.is_empty());
    parts
}

/// The `{…}` of a flow text: its inside, from the first `{` to the last
/// `}` — a `#…` tail or `- ` in front is not the map's.
fn flow_inner(text: &str) -> Option<&str> {
    let text = text.trim();
    let open = text.find('{')?;
    let close = text.rfind('}')?;
    (close > open).then(|| &text[open + 1..close])
}

/// A flow `{…}` as a row: JSON first (the runtime writes `- {"id":…}`
/// whole), else a shallow `key: value` split whose values keep only
/// scalars — a nested or tagged one leaves the row marked by its text
/// anyway.
fn flow_entry(text: &str) -> PatchRow {
    let mut row = PatchRow {
        dynamic: has_tag(text),
        ..PatchRow::default()
    };
    let Some(inner) = flow_inner(text) else {
        return row;
    };
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text.trim()) {
        row.id = map.get("id").and_then(Value::as_str).map(str::to_owned);
        row.title = map
            .get("title")
            .or_else(|| map.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        row.disabled = map.get("disabled").and_then(Value::as_bool);
        return row;
    }
    for part in flow_parts(inner) {
        if let Some((key, value)) = part.split_once(':') {
            match key.trim().trim_matches(['\'', '"']) {
                "id" => row.id = scalar(value),
                "disabled" => row.disabled = bool_scalar(value),
                "title" | "name" if row.title.is_none() => row.title = scalar(value),
                _ => {}
            }
        }
    }
    row
}

/// A `- {…}` item's rows: its own `id` is one; an `insert` key's value —
/// a `[{…}, …]` or one `{…}` — lists more. The runtime writes these
/// whole as `- {"…":…}`.
fn flow_item_rows(body: &str) -> Vec<PatchRow> {
    let mut rows = Vec::new();
    let entry = flow_entry(body);
    if entry.id.is_some() {
        rows.push(entry);
    }
    let Some(inner) = flow_inner(body) else {
        return rows;
    };
    for part in flow_parts(inner) {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        if key.trim().trim_matches(['\'', '"']) != "insert" {
            continue;
        }
        rows.extend(insert_value_rows(value.trim(), &[], 0));
    }
    rows
}

/// An `insert:`'s rows, its value written inline (`{…}` or `[{…}, …]`)
/// or the block under the key (`under`, at the key's `field_indent`).
fn insert_value_rows(value: &str, under: &[&str], field_indent: usize) -> Vec<PatchRow> {
    if value.starts_with('{') {
        let row = flow_entry(value);
        return if row.id.is_some() { vec![row] } else { Vec::new() };
    }
    if value.starts_with('[') {
        let inner = value[1..].strip_suffix(']').unwrap_or(&value[1..]);
        return flow_parts(inner)
            .iter()
            .filter(|part| part.trim_start().starts_with('{'))
            .filter_map(|part| {
                let row = flow_entry(part);
                row.id.is_some().then_some(row)
            })
            .collect();
    }
    // A scalar value rows nothing; an empty one (a `#…` comment aside)
    // reads its block.
    if value.is_empty() || value.starts_with('#') {
        return insert_rows(under, field_indent);
    }
    Vec::new()
}

/// An `insert:`'s block value as rows: the lines under the key are a
/// `- ` list of them (at the key's own indent or deeper), or one map.
fn insert_rows(lines: &[&str], field_indent: usize) -> Vec<PatchRow> {
    let Some(at) = lines
        .iter()
        .position(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    else {
        return Vec::new();
    };
    let first = lines[at].trim_end();
    let text = first.trim_start();
    let indent = indent_of(first);
    if text == "-" || text.starts_with("- ") {
        return list_rows(&lines[at..], indent);
    }
    // At the key's own indent or out of it, the line is a sibling's, not
    // the value's: the insert holds nothing.
    if indent <= field_indent {
        return Vec::new();
    }
    // One map: the block at its own indent; a dedent ends it.
    let end = lines[at + 1..]
        .iter()
        .position(|line| !line.trim().is_empty() && indent_of(line) < indent)
        .map(|i| at + 1 + i)
        .unwrap_or(lines.len());
    vec![entry_fields(text, &lines[at + 1..end], indent)]
}

/// A `- ` list at `dash` indent as rows: each dash opens one, the lines
/// indented past it its other fields, until the next dash or a dedent.
fn list_rows(lines: &[&str], dash: usize) -> Vec<PatchRow> {
    let mut rows = Vec::new();
    let mut fields: Vec<&str> = Vec::new();
    let mut first: Option<&str> = None;
    let mut field_indent = 0;
    for line in lines {
        let text = line.trim_end();
        if text.is_empty() || text.trim_start().starts_with('#') {
            continue;
        }
        let trimmed = text.trim_start();
        let indent = indent_of(text);
        if indent == dash && (trimmed == "-" || trimmed.starts_with("- ")) {
            if let Some(first) = first.take() {
                rows.push(entry_fields(first, &fields, field_indent));
                fields.clear();
            }
            let body = trimmed[1..].trim_start();
            field_indent = text.len() - body.len();
            first = Some(body);
        } else if indent > dash {
            fields.push(text);
        } else {
            break;
        }
    }
    if let Some(first) = first.take() {
        rows.push(entry_fields(first, &fields, field_indent));
    }
    rows
}

/// Which of `lines` an `insert:`'s value occupies, for scoping `!!js`
/// to the rows that carry it: the value is its key line plus the lines
/// indented past the key's `field_indent` (or `- ` items at it), so a
/// `- id:` row beside an insert isn't read as the insert's own.
fn insert_spans(lines: &[&str], field_indent: usize) -> Vec<bool> {
    let mut spans = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        let text = line.trim_end();
        if text.is_empty()
            || indent_of(text) != field_indent
            || !text.trim_start().starts_with("insert:")
        {
            continue;
        }
        spans[i] = true;
        for (j, next) in lines.iter().enumerate().skip(i + 1) {
            let text = next.trim_end();
            if text.is_empty() {
                continue;
            }
            let trimmed = text.trim_start();
            let indent = indent_of(next);
            if indent > field_indent
                || (indent == field_indent && (trimmed == "-" || trimmed.starts_with("- ")))
            {
                spans[j] = true;
            } else {
                break;
            }
        }
    }
    spans
}

/// The patch rows a `cordis.patch.yml`-shaped file lists: every row
/// under an `insert:` (a `- ` list of them, or one map), plus each
/// top-level `- ` item that keys on `id:` — a row that switches or
/// reshapes a row elsewhere, as a run's own `{id, disabled}` rows do.
fn patch_file_rows(yaml: &str) -> Vec<PatchRow> {
    let mut items: Vec<Vec<&str>> = Vec::new();
    let mut item: Vec<&str> = Vec::new();
    for line in yaml.lines() {
        // A top-level item opens only at the file's left edge: an
        // indented `- ` belongs to the item it's in.
        if line.starts_with("- ") || line == "-" {
            items.push(std::mem::take(&mut item));
        }
        item.push(line);
    }
    items.push(item);
    let mut rows = Vec::new();
    for item in items.into_iter().filter(|item| !item.is_empty()) {
        let dash = item[0];
        let (mut body, mut field_indent) = if dash == "-" {
            ("", 0)
        } else if let Some(rest) = dash.strip_prefix("- ") {
            (rest.trim(), dash.len() - rest.trim_start().len())
        } else {
            // Lines before the first item — comments, a `---` — list
            // nothing.
            continue;
        };
        let mut rest: &[&str] = &item[1..];
        if body.is_empty() {
            // `-` alone: its first field is the next line's.
            let Some(at) = rest.iter().position(|line| !line.trim().is_empty()) else {
                continue;
            };
            body = rest[at].trim();
            field_indent = indent_of(rest[at]);
            rest = &rest[at + 1..];
        }
        if body.starts_with('{') {
            rows.extend(flow_item_rows(body));
            continue;
        }
        // The item's own row, when it names an `id`. `!!js` inside an
        // `insert:`'s span belongs to the insert's rows, not this one's.
        let spans = insert_spans(rest, field_indent);
        let mut row = entry_fields(body, rest, field_indent);
        row.dynamic = has_tag(body)
            || rest
                .iter()
                .enumerate()
                .any(|(i, line)| !spans[i] && has_tag(line));
        if row.id.is_some() {
            rows.push(row);
        }
        // Every `insert:` key — the dash line's own, then each field's.
        if let Some(value) = body.strip_prefix("insert:") {
            rows.extend(insert_value_rows(value.trim(), rest, field_indent));
        }
        for (i, line) in rest.iter().enumerate() {
            let text = line.trim_end();
            if text.is_empty() || indent_of(text) != field_indent {
                continue;
            }
            if let Some(value) = text.trim_start().strip_prefix("insert:") {
                rows.extend(insert_value_rows(value.trim(), &rest[i + 1..], field_indent));
            }
        }
    }
    rows
}

/// The `disabled` each profile `cordis.patch.yml` row gives its `id`
/// (None: the row is there but sets none): later rows win, the file's
/// own order being how a patch applies. Top-level `id:` rows are what
/// dsh's manager writes; an `insert:` one counts the same.
fn patch_overrides(dir: &Path) -> std::collections::HashMap<String, Option<bool>> {
    let mut overrides = std::collections::HashMap::new();
    if let Ok(yaml) = std::fs::read_to_string(dir.join("cordis.patch.yml")) {
        for row in patch_file_rows(&yaml) {
            if let Some(id) = row.id {
                overrides.insert(id, row.disabled);
            }
        }
    }
    overrides
}

/// A patch path in `dsh.bundle.patch` resolved against the bundle's own
/// directory — never out of it: an installed package's `../` is its own
/// business elsewhere, not a file this read should follow.
fn patch_path(bundle_dir: &Path, path: &str) -> Option<PathBuf> {
    if Path::new(path).is_absolute() {
        return None;
    }
    let mut out = bundle_dir.to_path_buf();
    for part in Path::new(path).components() {
        match part {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() || !out.starts_with(bundle_dir) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

/// A bundle's patch files' rows, from its manifest's `dsh.bundle.patch`
/// (a path or a list of them, each read relative to the bundle's
/// directory). Unreadable or absent files row nothing.
fn bundle_patch_rows(dir: &Path, name: &str, manifest: &Value) -> Vec<PatchRow> {
    let bundle_dir = dir.join("node_modules").join(name);
    let patch = &manifest["dsh"]["bundle"]["patch"];
    let paths: Vec<&str> = match patch {
        Value::String(one) => vec![one.as_str()],
        Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let mut rows = Vec::new();
    for path in paths {
        let Some(path) = patch_path(&bundle_dir, path) else {
            continue;
        };
        if let Ok(yaml) = std::fs::read_to_string(path) {
            rows.extend(patch_file_rows(&yaml));
        }
    }
    rows
}

/// The `id` → default `disabled` every installed bundle's patch gives
/// its rows (absent is on): what an override row restates and dsh drops.
/// Later bundles win, sorted-name order being the profile's own.
fn row_defaults(home: &Path) -> std::collections::HashMap<String, bool> {
    let dir = profile_dir(home);
    let mut defaults = std::collections::HashMap::new();
    let Ok(bundles) = installed_bundles(home) else {
        return defaults;
    };
    for bundle in bundles {
        let Some(manifest) = read_json(&dir.join("node_modules").join(&bundle.name).join("package.json"))
            .ok()
            .flatten()
        else {
            continue;
        };
        for row in bundle_patch_rows(&dir, &bundle.name, &manifest) {
            if let Some(id) = row.id {
                defaults.insert(id, row.disabled.unwrap_or(false));
            }
        }
    }
    defaults
}

/// The rows a bundle's patch installs or adjusts, each with the profile's
/// override folded in: the page's per-row switches.
///
/// Like [`installed_bundles`] this is a read of files alone — a bundle
/// or patch that isn't there rows nothing, so it can't fail on a
/// missing one.
pub fn bundle_rows(home: &Path, name: &str) -> Result<Vec<BundleRow>, HarnessError> {
    let dir = profile_dir(home);
    let Some(manifest) = read_json(&dir.join("node_modules").join(name).join("package.json"))? else {
        return Ok(Vec::new());
    };
    let overrides = patch_overrides(&dir);
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for row in bundle_patch_rows(&dir, name, &manifest) {
        // A row that names no id can't be switched — there is nothing
        // to write the override against — and lists as nothing rather
        // than a switch without a name.
        let Some(id) = row.id else {
            continue;
        };
        // First read stands: a second patch op for one id is the same
        // row, not another.
        if !seen.insert(id.clone()) {
            continue;
        }
        let set = overrides.get(&id).copied().flatten();
        let editable = !row.dynamic && writable_id(&id);
        rows.push(BundleRow {
            disabled: set.or(row.disabled).unwrap_or(false),
            overridden: set.is_some(),
            note: if editable {
                None
            } else if row.dynamic {
                Some("it carries a !!js expression, which can't be switched safely".into())
            } else {
                Some("its id can't be written to cordis.patch.yml safely".into())
            },
            editable,
            id,
            title: row.title,
        });
    }
    Ok(rows)
}

/// Whether `id` can sit in a `- id: <id>` line and read back: anything
/// but a line break, which quoting keeps safe but no real id carries.
fn writable_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.contains(['\n', '\r'])
}

/// `id` after `- id: `: plain when nothing in it reads as YAML syntax
/// (a leading `-`, `@` or indicator, a `:` or `#` inside, a `true` or
/// number that would read as a value), else a JSON string — a YAML
/// double-quoted scalar word for word.
fn id_scalar(id: &str) -> String {
    let plain = !id.is_empty()
        && !matches!(id, "true" | "false" | "null" | "~")
        && id.parse::<f64>().is_err()
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '/'));
    if plain {
        id.to_owned()
    } else {
        serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into())
    }
}

/// One `key: value` line of a block row, where it sits for an edit.
#[derive(Debug, Clone)]
struct FieldLine {
    /// The line's first byte.
    start: usize,
    /// Its content's end, the newline not counted.
    content_end: usize,
    /// Its end, the newline counted: where the line comes out whole.
    line_end: usize,
    /// It is the `- ` line's own field.
    on_dash: bool,
    /// Its leading whitespace, for the same line written again.
    indent: usize,
    /// The `key: value` text itself.
    text: String,
    key: String,
    /// The value's raw text, for [`scalar`].
    value: String,
}

/// `line`'s `key: value` as a [`FieldLine`], `field` being the text the
/// key is read from (for a `- ` line, what follows the dash).
fn push_field(fields: &mut Vec<FieldLine>, at: usize, line: &str, field: &str, on_dash: bool) {
    let text = line.trim_end();
    let Some((key, value)) = field.split_once(':') else {
        return;
    };
    fields.push(FieldLine {
        start: at,
        content_end: at + text.len(),
        line_end: at + line.len(),
        on_dash,
        indent: text.len() - text.trim_start().len(),
        text: field.to_owned(),
        key: key.trim().to_owned(),
        value: value.trim().to_owned(),
    });
}

/// The top-level `- ` item in `yaml` whose `id:` is `id`: its byte
/// range, where its `disabled:` sits (or a new one lands), and what
/// else it fields. A row the line read can't answer for — `!!js`, or a
/// flow it can't write back — refuses the whole write rather than
/// rewrite what it can't verify.
struct OverrideRow {
    /// The item's first byte: its `- ` line's start.
    start: usize,
    /// The end of its last content line, newline counted: where the row
    /// comes out whole. Blank lines and file-level comments after it
    /// are the file's, not the row's.
    end: usize,
    /// The end of the last content char: where a field line lands.
    content_end: usize,
    /// Its keys' indent, for a `disabled:` to append.
    field_indent: usize,
    /// Its `disabled:` field, when it has one.
    disabled: Option<FieldLine>,
    /// Its first field that isn't `disabled:` — the one a `- ` line
    /// emptied of `disabled:` then carries.
    first_kept: Option<FieldLine>,
    /// Field lines besides `id` and `disabled`: any left keeps a row
    /// that lost its `disabled:`.
    other_fields: usize,
    /// The flow `{…}`'s `key: value` parts in order — (key, the value's
    /// raw text) — plus the byte just past its `}`, for what trails it
    /// (a comment, the newline).
    flow: Option<(Vec<(String, String)>, usize)>,
}

struct YamlEvents(Vec<Event>);

impl EventReceiver for YamlEvents {
    fn on_event(&mut self, event: Event) {
        self.0.push(event);
    }
}

fn yaml_events(yaml: &str) -> Option<Vec<Event>> {
    let mut parser = Parser::new_from_str(yaml);
    let mut events = YamlEvents(Vec::new());
    parser.load(&mut events, true).ok()?;
    Some(events.0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum YamlRoot {
    Empty,
    Sequence,
    Mapping,
    Other,
}

/// The root node of a one-document YAML stream. Anything before or after it —
/// a second document, in particular — makes the stream `Other`, because these
/// editors only know how to place a row in one plain document.
fn yaml_root(events: &[Event]) -> YamlRoot {
    let mut index = usize::from(matches!(events.first(), Some(Event::StreamStart)));
    if matches!(events.get(index), Some(Event::StreamEnd)) {
        return YamlRoot::Empty;
    }
    if matches!(events.get(index), Some(Event::DocumentStart)) {
        index += 1;
    }
    if matches!(events.get(index), Some(Event::DocumentEnd)) {
        return YamlRoot::Empty;
    }
    let root = match events.get(index) {
        Some(Event::SequenceStart(..)) => YamlRoot::Sequence,
        Some(Event::MappingStart(..)) => YamlRoot::Mapping,
        Some(Event::Scalar(value, ..)) if value.is_empty() => YamlRoot::Empty,
        Some(Event::Scalar(..) | Event::Alias(..)) => YamlRoot::Other,
        _ => return YamlRoot::Other,
    };
    if matches!(events.get(index), Some(Event::Scalar(..) | Event::Alias(..))) {
        index += 1;
    } else {
        let mut depth = 0usize;
        while index < events.len() {
            match events[index] {
                Event::SequenceStart(..) | Event::MappingStart(..) => depth += 1,
                Event::SequenceEnd | Event::MappingEnd => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        index += 1;
                        break;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        if depth != 0 {
            return YamlRoot::Other;
        }
    }
    if matches!(events.get(index), Some(Event::DocumentEnd)) {
        index += 1;
    }
    if index + 1 == events.len() && matches!(events.get(index), Some(Event::StreamEnd)) {
        root
    } else {
        YamlRoot::Other
    }
}

fn scan_yaml_id(events: &[Event], mut index: usize, needle: &str) -> (bool, usize) {
    match events.get(index) {
        Some(Event::Scalar(..) | Event::Alias(..)) => return (false, index + 1),
        Some(Event::SequenceStart(..)) => {
            index += 1;
            let mut found = false;
            while index < events.len() && !matches!(events[index], Event::SequenceEnd) {
                let (hit, next) = scan_yaml_id(events, index, needle);
                found |= hit;
                index = next;
            }
            (found, index + usize::from(index < events.len()))
        }
        Some(Event::MappingStart(..)) => {
            index += 1;
            let mut found = false;
            while index < events.len() && !matches!(events[index], Event::MappingEnd) {
                let key_is_id = matches!(&events[index], Event::Scalar(value, ..) if value == "id");
                let (hit, after_key) = scan_yaml_id(events, index, needle);
                let value_is_id = key_is_id
                    && matches!(events.get(after_key), Some(Event::Scalar(value, ..)) if value == needle);
                let (hit_value, after_value) = scan_yaml_id(events, after_key, needle);
                found |= hit || hit_value || value_is_id;
                index = after_value;
            }
            (found, index + usize::from(index < events.len()))
        }
        _ => (false, index + 1),
    }
}

fn yaml_has_id_value(events: &[Event], needle: &str) -> bool {
    let mut index = usize::from(matches!(events.first(), Some(Event::StreamStart)));
    if matches!(events.get(index), Some(Event::DocumentStart)) {
        index += 1;
    }
    scan_yaml_id(events, index, needle).0
}

fn skip_yaml_node(events: &[Event], mut index: usize) -> Option<usize> {
    match events.get(index) {
        Some(Event::Scalar(..) | Event::Alias(..)) => return Some(index + 1),
        Some(Event::SequenceStart(..) | Event::MappingStart(..)) => {}
        _ => return None,
    }
    let mut depth = 0usize;
    while index < events.len() {
        match events[index] {
            Event::SequenceStart(..) | Event::MappingStart(..) => depth += 1,
            Event::SequenceEnd | Event::MappingEnd => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// How many times `needle` keys the root mapping, including quoted spellings.
/// Values elsewhere in the file are not keys and never make an edit refuse.
fn root_mapping_key_count(events: &[Event], needle: &str) -> usize {
    let mut index = usize::from(matches!(events.first(), Some(Event::StreamStart)));
    if matches!(events.get(index), Some(Event::DocumentStart)) {
        index += 1;
    }
    if !matches!(events.get(index), Some(Event::MappingStart(..))) {
        return 0;
    }
    index += 1;
    let mut count = 0;
    while index < events.len() && !matches!(events[index], Event::MappingEnd) {
        if matches!(&events[index], Event::Scalar(value, ..) if value == needle) {
            count += 1;
        }
        let Some(after_key) = skip_yaml_node(events, index) else {
            return count;
        };
        let Some(after_value) = skip_yaml_node(events, after_key) else {
            return count;
        };
        index = after_value;
    }
    count
}

/// The override file's YAML guard: valid syntax, one root sequence, and
/// whether the scalar `id` appears anywhere in it. The last answer is only a
/// conservative unsupported-shape check — if the line parser cannot find that
/// same id, the write refuses rather than adding a duplicate row.
fn override_yaml_guard(yaml: &str, id: &str) -> Option<(bool, bool)> {
    let events = yaml_events(yaml)?;
    let root = yaml_root(&events);
    Some((
        matches!(root, YamlRoot::Empty | YamlRoot::Sequence),
        yaml_has_id_value(&events, id),
    ))
}

/// A root flow sequence on the first content line: `(empty, start, end)` for
/// its source text. Block rows can append after a block sequence, but not
/// after the closing `]` of a flow sequence.
fn flow_sequence_root(yaml: &str) -> Option<(bool, usize, usize)> {
    let mut at = 0;
    for line in yaml.split_inclusive('\n') {
        let text = line.trim_end();
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" {
            at += line.len();
            continue;
        }
        return trimmed.starts_with('[').then(|| {
            let start = at + line.find('[').unwrap_or(0);
            (trimmed == "[]", start, at + text.len())
        });
    }
    None
}

/// The override file's row for `id`, when one top-level `- ` item keys
/// on it.
fn override_row(yaml: &str, id: &str) -> Result<Option<OverrideRow>, HarnessError> {
    let mut starts = Vec::new();
    let mut at = 0;
    for line in yaml.split_inclusive('\n') {
        let text = line.trim_end();
        if text.starts_with("- ") || text == "-" {
            starts.push(at);
        }
        at += line.len();
    }
    for (n, start) in starts.iter().enumerate() {
        let (start, end) = (*start, starts.get(n + 1).copied().unwrap_or(yaml.len()));
        let item = &yaml[start..end];
        let mut lines: Vec<(usize, &str)> = Vec::new();
        let mut content_end = start;
        let mut row_end = start;
        let mut off = start;
        for line in item.split_inclusive('\n') {
            lines.push((off, line));
            let text = line.trim_end();
            // Blank lines and file-level comments aren't the row's.
            if !text.is_empty() && !(indent_of(text) == 0 && text.trim_start().starts_with('#')) {
                content_end = off + text.len();
                row_end = off + line.len();
            }
            off += line.len();
        }
        let Some(&(dash_at, dash)) = lines.first() else {
            continue;
        };
        let (body, mut field_indent) = if dash.trim_end() == "-" {
            ("", 0)
        } else if let Some(rest) = dash.strip_prefix("- ") {
            (rest.trim(), dash.len() - rest.trim_start().len())
        } else {
            continue;
        };
        let mut fields: Vec<FieldLine> = Vec::new();
        if body.is_empty() {
            // `-` alone: its first field is the next line's.
            let Some(at) = lines[1..]
                .iter()
                .position(|(_, line)| !line.trim().is_empty())
            else {
                continue;
            };
            field_indent = indent_of(lines[1 + at].1);
            for (off, line) in &lines[1 + at..] {
                let text = line.trim_end();
                if text.is_empty() || indent_of(text) != field_indent {
                    continue;
                }
                push_field(&mut fields, *off, line, text.trim_start(), false);
            }
        } else {
            push_field(&mut fields, dash_at, dash, body, true);
            for (off, line) in &lines[1..] {
                let text = line.trim_end();
                if text.is_empty() || indent_of(text) != field_indent {
                    continue;
                }
                push_field(&mut fields, *off, line, text.trim_start(), false);
            }
        }
        if body.starts_with('{') {
            // A flow row edits only when its whole `{…}` is JSON — what
            // is written back stays what was read.
            let parts: Vec<(String, String)> = flow_inner(body)
                .map(flow_parts)
                .unwrap_or_default()
                .into_iter()
                .map(|part| {
                    let (key, value) = part.split_once(':').unwrap_or(("", part.as_str()));
                    (
                        key.trim().trim_matches(['\'', '"']).to_owned(),
                        value.trim().to_owned(),
                    )
                })
                .collect();
            if !parts
                .iter()
                .any(|(key, value)| key == "id" && scalar(value).as_deref() == Some(id))
            {
                continue;
            }
            if !matches!(serde_json::from_str::<Value>(body), Ok(Value::Object(_))) || has_tag(item)
            {
                return Err(HarnessError::Protocol(format!(
                    "cordis.patch.yml: the row for {id:?} can't be edited safely"
                )));
            }
            let flow_end = dash_at
                + dash
                    .rfind('}')
                    .map(|at| at + 1)
                    .unwrap_or(dash.trim_end().len());
            return Ok(Some(OverrideRow {
                start,
                end: row_end,
                content_end,
                field_indent,
                disabled: None,
                first_kept: None,
                other_fields: 0,
                flow: Some((parts, flow_end)),
            }));
        }
        if !fields
            .iter()
            .any(|field| field.key == "id" && scalar(&field.value).as_deref() == Some(id))
        {
            continue;
        }
        if has_tag(item) {
            return Err(HarnessError::Protocol(format!(
                "cordis.patch.yml: the row for {id:?} carries !!js and can't be edited safely"
            )));
        }
        return Ok(Some(OverrideRow {
            start,
            end: row_end,
            content_end,
            field_indent,
            disabled: fields.iter().rev().find(|field| field.key == "disabled").cloned(),
            first_kept: fields
                .iter()
                .find(|field| field.key != "disabled" && !field.on_dash)
                .cloned(),
            other_fields: fields
                .iter()
                .filter(|field| field.key != "id" && field.key != "disabled")
                .count(),
            flow: None,
        }));
    }
    Ok(None)
}

/// Set a row's `disabled` in the profile's `cordis.patch.yml`, the way
/// dsh's plugin manager does: the override is a `- id: <id>` row, its
/// `disabled:` updated in place or appended, the rest of the row — and
/// file — byte for byte as it was.
///
/// When the ask only restates the row's own default (what the bundle's
/// patch gives it; absent is on), the override comes out instead: the
/// `disabled:` line, and the whole row when `id` was all it carried.
/// A row the line read can't safely edit — `!!js`, or a flow it can't
/// write back — refuses before anything is written.
pub fn set_row_disabled(home: &Path, id: &str, disabled: bool) -> Result<(), HarnessError> {
    if !writable_id(id) {
        return Err(HarnessError::Protocol(format!("invalid patch row id: {id:?}")));
    }
    let dir = profile_dir(home);
    let path = dir.join("cordis.patch.yml");
    let yaml = match std::fs::read_to_string(&path) {
        Ok(yaml) => yaml,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let row = override_row(&yaml, id)?;
    let flow_root = flow_sequence_root(&yaml);
    if matches!(flow_root, Some((false, _, _))) {
        return Err(HarnessError::Protocol(
            "cordis.patch.yml: a non-empty flow sequence can't be edited safely".into(),
        ));
    }
    match override_yaml_guard(&yaml, id) {
        Some((true, mentioned)) if row.is_some() || !mentioned => {}
        Some((true, _)) => {
            return Err(HarnessError::Protocol(format!(
                "cordis.patch.yml: the row for {id:?} uses YAML outside the editable subset"
            )));
        }
        Some((false, _)) => {
            return Err(HarnessError::Protocol(
                "cordis.patch.yml: only one top-level sequence of rows can be edited safely".into(),
            ));
        }
        None => {
            return Err(HarnessError::Protocol(
                "cordis.patch.yml: malformed YAML can't be edited safely".into(),
            ));
        }
    }
    let default = row_defaults(home).get(id).copied().unwrap_or(false);
    let out = if disabled == default {
        let Some(row) = row else {
            // Restating the default with no row to restate it on.
            return Ok(());
        };
        clear_disabled(&yaml, &row)
    } else {
        match row {
            None => {
                let row_text = format!("- id: {}\n  disabled: {disabled}", id_scalar(id));
                if let Some((true, start, end)) = flow_root {
                    let mut out = yaml.clone();
                    out.replace_range(start..end, &row_text);
                    out
                } else {
                    let mut out = yaml.clone();
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&row_text);
                    out.push('\n');
                    out
                }
            }
            Some(row) => set_disabled(&yaml, &row, disabled)?,
        }
    };
    if out != yaml {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, out)?;
    }
    Ok(())
}

/// A flow field's value written back: what was already JSON stays raw
/// (the writeback only ever runs on a `{…}` that parsed whole), and a
/// bare word quotes.
fn json_scalar(value: &str) -> String {
    if serde_json::from_str::<Value>(value).is_ok() {
        value.to_owned()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
    }
}

/// `parts` back as a `{…}`.
fn flow_write(parts: &[(String, String)]) -> String {
    let parts: Vec<String> = parts
        .iter()
        .map(|(key, value)| {
            format!(
                "{}:{}",
                serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()),
                json_scalar(value)
            )
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// The file without `row`'s `disabled:` — just the line, or the row
/// whole when `id` was all it carried.
fn clear_disabled(yaml: &str, row: &OverrideRow) -> String {
    if let Some((parts, flow_end)) = &row.flow {
        let kept: Vec<(String, String)> = parts
            .iter()
            .filter(|(key, _)| key != "disabled")
            .cloned()
            .collect();
        if kept.iter().all(|(key, _)| key == "id") {
            return format!("{}{}", &yaml[..row.start], &yaml[row.end..]);
        }
        return format!(
            "{}- {}{}",
            &yaml[..row.start],
            flow_write(&kept),
            &yaml[*flow_end..]
        );
    }
    if row.other_fields == 0 {
        // `id` is all the row leaves — with or without its `disabled:`,
        // it goes whole.
        return format!("{}{}", &yaml[..row.start], &yaml[row.end..]);
    }
    let Some(field) = &row.disabled else {
        // No `disabled:` to drop: the file already says the default.
        return yaml.to_owned();
    };
    if field.on_dash {
        // `- disabled:` emptied of itself carries the next field up.
        let kept = row.first_kept.clone().expect("other fields counted one");
        return format!(
            "{}- {}{}{}",
            &yaml[..row.start],
            kept.text,
            &yaml[field.content_end..kept.start],
            &yaml[kept.line_end..]
        );
    }
    let mut out = yaml.to_owned();
    out.replace_range(field.start..field.line_end, "");
    out
}

/// `row`'s `disabled:` set, its line rewritten or a new one landed at
/// the row's field indent — nothing else in the file touched.
fn set_disabled(yaml: &str, row: &OverrideRow, disabled: bool) -> Result<String, HarnessError> {
    if let Some((parts, flow_end)) = &row.flow {
        let mut parts = parts.clone();
        let mut wrote = false;
        for part in &mut parts {
            if part.0 == "disabled" {
                part.1 = disabled.to_string();
                wrote = true;
            }
        }
        if !wrote {
            parts.push(("disabled".into(), disabled.to_string()));
        }
        return Ok(format!(
            "{}- {}{}",
            &yaml[..row.start],
            flow_write(&parts),
            &yaml[*flow_end..]
        ));
    }
    let mut out = yaml.to_owned();
    if let Some(field) = &row.disabled {
        // A ` #…` tail the line carried stays carried.
        let comment = field.text.find(" #").map_or("", |at| &field.text[at..]);
        let line = if field.on_dash {
            format!("- disabled: {disabled}{comment}")
        } else {
            format!("{}disabled: {disabled}{comment}", " ".repeat(field.indent))
        };
        out.replace_range(field.start..field.content_end, &line);
        return Ok(out);
    }
    out.insert_str(
        row.content_end,
        &format!("\n{}disabled: {disabled}", " ".repeat(row.field_indent)),
    );
    Ok(out)
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
    let block = allow_builds(&yaml);
    match yaml_events(&yaml).map(|events| {
        (
            matches!(yaml_root(&events), YamlRoot::Empty | YamlRoot::Mapping),
            root_mapping_key_count(&events, "allowBuilds"),
        )
    }) {
        Some((true, count)) if count == usize::from(block.is_some()) => {}
        Some((true, _)) => {
            return Err(HarnessError::Protocol(
                "pnpm-workspace.yaml: allowBuilds uses YAML outside the editable subset".into(),
            ));
        }
        Some((false, _)) => {
            return Err(HarnessError::Protocol(
                "pnpm-workspace.yaml: only one top-level mapping can be edited safely".into(),
            ));
        }
        None => {
            return Err(HarnessError::Protocol(
                "pnpm-workspace.yaml: malformed YAML can't be edited safely".into(),
            ));
        }
    }
    let key = build_key(package);
    let out = match block {
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

    #[test]
    fn allow_build_yaml_outside_the_editable_subset_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        for (limit, yaml) in [
            // Inline maps are editable only when the whole `{…}` is on one line.
            (
                "a multiline inline map",
                "allowBuilds: {\n  esbuild: true\n}\n",
            ),
            // A scalar is not an allowBuilds map; appending another key would corrupt it.
            ("a scalar value", "allowBuilds: true\n"),
            // The line editor cannot answer which duplicate key pnpm would read.
            (
                "duplicate keys",
                "allowBuilds:\n  esbuild: true\nallowBuilds:\n  other: true\n",
            ),
            // Appending `allowBuilds:` under a sequence root would corrupt it.
            ("a sequence root", "- allowBuilds\n"),
            // Malformed YAML is never edited by line positions.
            ("malformed yaml", "allowBuilds:\n  esbuild: [true\n"),
        ] {
            write(&workspace(dir.path()), yaml);
            let before = read(dir.path());
            assert!(
                matches!(
                    allow_build(dir.path(), "dsh-hello-plugin"),
                    Err(HarnessError::Protocol(_))
                ),
                "{limit} should refuse"
            );
            assert_eq!(read(dir.path()), before, "{limit} changed");
        }
    }

    fn profile_patch(home: &Path, contents: &str) {
        write(&profile_dir(home).join("cordis.patch.yml"), contents);
    }

    fn read_profile_patch(home: &Path) -> String {
        std::fs::read_to_string(profile_dir(home).join("cordis.patch.yml")).unwrap()
    }

    fn read_bundle_patch(home: &Path, name: &str) -> String {
        std::fs::read_to_string(
            profile_dir(home)
                .join("node_modules")
                .join(name)
                .join("cordis.patch.yml"),
        )
        .unwrap()
    }

    fn patched_bundle(home: &Path, name: &str, patch_files: &str, contents: &str) {
        profile(
            home,
            &format!(
                r#"{{"dependencies":{{"{name}":"file:/abs/path"}},
                    "dsh":{{"profile":{{"bundles":["{name}"]}}}}}}"#
            ),
        );
        bundle(
            home,
            name,
            &format!(
                r#"{{"name":"{name}","dsh":{{"bundle":{{"patch":{patch_files}}}}}}}"#
            ),
        );
        patch(home, name, contents);
    }

    #[test]
    fn bundle_rows_reads_every_patch_file() {
        let dir = tempfile::tempdir().unwrap();
        patched_bundle(
            dir.path(),
            "dsh-two-patches",
            r#"["./cordis.patch.yml","./more.patch.yml"]"#,
            "- insert:\n    - id: one\n      title: 'First row'\n      name: 'pkg-one'\n",
        );
        write(
            &profile_dir(dir.path())
                .join("node_modules")
                .join("dsh-two-patches")
                .join("more.patch.yml"),
            "- insert:\n    - id: two\n      disabled: true\n- id: three\n  disabled: false\n",
        );
        let rows = bundle_rows(dir.path(), "dsh-two-patches").unwrap();
        assert_eq!(
            rows,
            vec![
                BundleRow {
                    id: "one".into(),
                    title: Some("First row".into()),
                    disabled: false,
                    overridden: false,
                    editable: true,
                    note: None,
                },
                BundleRow {
                    id: "two".into(),
                    title: None,
                    disabled: true,
                    overridden: false,
                    editable: true,
                    note: None,
                },
                BundleRow {
                    id: "three".into(),
                    title: None,
                    disabled: false,
                    overridden: false,
                    editable: true,
                    note: None,
                },
            ]
        );
    }

    #[test]
    fn a_rows_own_disabled_is_its_default_and_the_profiles_wins() {
        let dir = tempfile::tempdir().unwrap();
        patched_bundle(
            dir.path(),
            "dsh-defaults",
            r#""./cordis.patch.yml""#,
            "- insert:\n    - id: off-by-default\n      disabled: true\n    - id: on\n",
        );
        profile_patch(
            dir.path(),
            "- id: on\n  disabled: true\n- id: off-by-default\n  disabled: false\n",
        );
        let rows = bundle_rows(dir.path(), "dsh-defaults").unwrap();
        assert_eq!(
            rows.iter().map(|r| (r.id.as_str(), r.disabled, r.overridden)).collect::<Vec<_>>(),
            vec![("off-by-default", false, true), ("on", true, true)]
        );
    }

    #[test]
    fn set_row_disabled_keeps_the_rows_other_fields_and_the_files() {
        let dir = tempfile::tempdir().unwrap();
        patched_bundle(
            dir.path(),
            "dsh-defaults",
            r#""./cordis.patch.yml""#,
            "- insert:\n    - id: llm-pi-ai\n      disabled: true\n",
        );
        let before = "- id: llm-pi-ai\n  name: '@scope/llm-pi-ai'\n  disabled: true\n  config:\n    models:\n      - deepseek\n- id: other\n  disabled: false\n";
        profile_patch(dir.path(), before);
        set_row_disabled(dir.path(), "llm-pi-ai", false).unwrap();
        assert_eq!(
            read_profile_patch(dir.path()),
            "- id: llm-pi-ai\n  name: '@scope/llm-pi-ai'\n  disabled: false\n  config:\n    models:\n      - deepseek\n- id: other\n  disabled: false\n"
        );
        // A row the file has none of appends at the end.
        set_row_disabled(dir.path(), "tool-fs", true).unwrap();
        assert_eq!(
            read_profile_patch(dir.path()),
            "- id: llm-pi-ai\n  name: '@scope/llm-pi-ai'\n  disabled: false\n  config:\n    models:\n      - deepseek\n- id: other\n  disabled: false\n- id: tool-fs\n  disabled: true\n"
        );
    }

    #[test]
    fn an_override_that_restates_the_default_comes_out() {
        let dir = tempfile::tempdir().unwrap();
        patched_bundle(
            dir.path(),
            "dsh-defaults",
            r#""./cordis.patch.yml""#,
            "- insert:\n    - id: off\n      disabled: true\n",
        );
        profile_patch(
            dir.path(),
            "- id: off\n  disabled: true\n- id: keep\n  disabled: false\n  name: keep-plugin\n",
        );
        // `off` defaults disabled: the override only restates it, so the
        // whole row — `id` was all it carried — comes out.
        set_row_disabled(dir.path(), "off", true).unwrap();
        assert_eq!(
            read_profile_patch(dir.path()),
            "- id: keep\n  disabled: false\n  name: keep-plugin\n"
        );
        // `keep` defaults on (no bundle row for it): dropping its
        // `disabled:` leaves the row's other fields standing.
        set_row_disabled(dir.path(), "keep", false).unwrap();
        assert_eq!(read_profile_patch(dir.path()), "- id: keep\n  name: keep-plugin\n");
    }

    #[test]
    fn a_row_with_js_is_listed_but_not_editable_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        patched_bundle(
            dir.path(),
            "dsh-dynamic",
            r#""./cordis.patch.yml""#,
            "- insert:\n    - id: mcp\n      name: 'dsh-mcp'\n      config: !!js JSON.parse(process.env.X)\n    - id: plain\n",
        );
        let rows = bundle_rows(dir.path(), "dsh-dynamic").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "mcp");
        assert!(!rows[0].editable);
        assert!(rows[0].note.is_some());
        assert!(rows[1].editable);
        let before = read_bundle_patch(dir.path(), "dsh-dynamic");
        // The bundle's file is never touched by a row write anywhere.
        set_row_disabled(dir.path(), "plain", true).unwrap();
        assert_eq!(read_bundle_patch(dir.path(), "dsh-dynamic"), before);
    }

    #[test]
    fn a_row_it_cant_write_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        for yaml in [
            "- id: dyn\n  disabled: !!js true\n  name: x\n",
            "- {id: 'dyn', disabled: true}\n",
        ] {
            profile_patch(dir.path(), yaml);
            let before = read_profile_patch(dir.path());
            assert!(matches!(
                set_row_disabled(dir.path(), "dyn", false),
                Err(HarnessError::Protocol(_))
            ));
            assert_eq!(read_profile_patch(dir.path()), before);
        }
        assert!(matches!(
            set_row_disabled(dir.path(), "bad\nid", true),
            Err(HarnessError::Protocol(_))
        ));
    }

    #[test]
    fn empty_and_header_only_patch_documents_can_gain_rows() {
        let dir = tempfile::tempdir().unwrap();
        profile_patch(dir.path(), "# patch comments\n[]\n");
        set_row_disabled(dir.path(), "dyn", true).unwrap();
        assert_eq!(
            read_profile_patch(dir.path()),
            "# patch comments\n- id: dyn\n  disabled: true\n"
        );

        profile_patch(dir.path(), "---\n- id: dyn\n  disabled: true\n");
        set_row_disabled(dir.path(), "dyn", false).unwrap();
        assert_eq!(read_profile_patch(dir.path()), "---\n");
        set_row_disabled(dir.path(), "dyn", true).unwrap();
        assert_eq!(
            read_profile_patch(dir.path()),
            "---\n- id: dyn\n  disabled: true\n"
        );
    }

    #[test]
    fn an_id_in_another_rows_title_does_not_block_a_new_override() {
        let dir = tempfile::tempdir().unwrap();
        profile_patch(dir.path(), "- id: other\n  title: dyn\n");
        set_row_disabled(dir.path(), "dyn", true).unwrap();
        assert_eq!(
            read_profile_patch(dir.path()),
            "- id: other\n  title: dyn\n- id: dyn\n  disabled: true\n"
        );
    }

    #[test]
    fn yaml_outside_the_editable_subset_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        for (limit, yaml) in [
            // The anchor makes the line read unable to prove this is `id: dyn`.
            ("an anchored id", "- id: &row dyn\n  disabled: true\n"),
            // A flow row is editable only when its whole one-line object is JSON.
            (
                "a multiline flow row",
                "- {\"id\": \"dyn\",\n  \"disabled\": true}\n",
            ),
            // Patch rows nested in another mapping are not top-level switch rows.
            ("a nested row", "- wrapper:\n    - id: dyn\n"),
            // The editor owns one document, not a stream of them.
            (
                "multiple documents",
                "---\n- id: other\n---\n- id: dyn\n",
            ),
            // Appending `- id:` under a mapping root would corrupt the document.
            ("a mapping root", "rows:\n  - id: dyn\n"),
            // Malformed YAML is never edited by line positions.
            ("malformed yaml", "- id: dyn\n  disabled: [true\n"),
        ] {
            profile_patch(dir.path(), yaml);
            let before = read_profile_patch(dir.path());
            assert!(
                matches!(
                    set_row_disabled(dir.path(), "dyn", false),
                    Err(HarnessError::Protocol(_))
                ),
                "{limit} should refuse"
            );
            assert_eq!(read_profile_patch(dir.path()), before, "{limit} changed");
        }
    }
}
