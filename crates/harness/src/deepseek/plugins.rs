//! Read-only view of the dsh bundles installed in Zeron's `sdk` profile.
//!
//! The profile is a plain pnpm project (`profiles/sdk/package.json` plus its
//! `node_modules`), so listing what is installed is a read of two manifests per
//! bundle and nothing else: no pnpm, no dsh, no writes.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::HarnessError;

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

#[cfg(test)]
mod tests {
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
}
