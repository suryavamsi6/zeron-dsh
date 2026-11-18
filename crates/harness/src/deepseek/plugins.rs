//! The dsh bundles installed in Zeron's `sdk` profile, and the command that
//! installs or removes one.
//!
//! The profile is a plain pnpm project (`profiles/sdk/package.json` plus its
//! `node_modules`), so listing what is installed is a read of two manifests per
//! bundle and nothing else: no pnpm, no dsh, no writes. Changing it is dsh's
//! own `plugin` subcommand, built here and run by the caller.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde_json::Value;

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
                version: text(&manifest, "version"),
                engines_dsh: text(&manifest["engines"], "dsh"),
                description: text(&manifest, "description"),
            }
        })
        .collect();
    bundles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(bundles)
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
        assert_eq!(bundles[1].version, None);
        assert_eq!(bundles[1].engines_dsh, None);
        assert_eq!(bundles[1].description, None);
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
}
