//! The dsh bundles installed in this device's `sdk` profile, and the two
//! operations that change it.
//!
//! Listing is a read of the profile's manifests and nothing else: no pnpm, no
//! dsh, no writes. Adding and removing run dsh's own `plugin` subcommand, one
//! at a time — the profile is a single pnpm project, so two installs at once
//! would race over the same `node_modules`.

use std::path::Path;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use zeron_harness::deepseek::{
    plugins::{
        allow_build, bundle_rows, engines_refusal, installed_bundles, pnpm_available,
        run_plugin_op, set_row_disabled, valid_spec, BundleRow, PluginOp, PluginRun,
    },
    DeepSeekHarness,
};
use zeron_rpc::{methods, RpcError};

/// One `dsh plugin` run at a time: pnpm writes the whole profile, so a second
/// install started while the first is running would corrupt it. A row write
/// joins it: the profile's cordis.patch.yml is a file an install rewrites.
static INSTALL: Mutex<()> = Mutex::const_new(());

/// Whether `method` is answered here.
pub fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::DSH_PLUGINS
            | methods::DSH_PLUGIN_ADD
            | methods::DSH_PLUGIN_REMOVE
            | methods::DSH_PLUGIN_ROWS
            | methods::DSH_PLUGIN_SET_ROW
    )
}

/// Answer one of the five methods.
pub async fn call(
    harness: &DeepSeekHarness,
    method: &str,
    params: &Value,
) -> Result<Value, RpcError> {
    match method {
        methods::DSH_PLUGINS => list(harness.home()),
        methods::DSH_PLUGIN_ROWS => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::BadParams("rows takes a name string".into()))?;
            let rows = bundle_rows(harness.home(), name)
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            Ok(json!({ "rows": rows_json(rows) }))
        }
        methods::DSH_PLUGIN_SET_ROW => {
            let (id, disabled) = set_row(params)?;
            // Held across the write, as an install is: the profile's
            // cordis.patch.yml is the same file an install rewrites.
            let _running = INSTALL.lock().await;
            set_row_disabled(harness.home(), id, disabled)
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            let rows = rows_owning(harness.home(), id)
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            Ok(json!({ "rows": rows_json(rows) }))
        }
        methods::DSH_PLUGIN_ADD | methods::DSH_PLUGIN_REMOVE => {
            let op = plugin_op(method, params)?;
            // Held across the run, not just the spawn: the profile is one
            // pnpm project and the child owns it until it exits.
            let _running = INSTALL.lock().await;
            // The caller's go-ahead for one package's build scripts lands in
            // the profile's pnpm-workspace.yaml before the run that needs it.
            if method == methods::DSH_PLUGIN_ADD {
                if let Some(package) = params.get("allowBuild").and_then(Value::as_str) {
                    allow_build(harness.home(), package)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                }
            }
            let run = run_plugin_op(harness.sdk_root(), harness.home(), op)
                .await
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            Ok(json!({
                "ok": run.ok,
                "exitCode": run.exit_code,
                "log": run.log_tail,
                "refusal": engines_refusal(&run.log_tail),
                "needsBuildApproval": build_approval(&run),
            }))
        }
        other => Err(RpcError::UnknownMethod(other.into())),
    }
}

/// A row's fields as the page reads them.
fn rows_json(rows: Vec<BundleRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "title": row.title,
                "disabled": row.disabled,
                "overridden": row.overridden,
                "editable": row.editable,
                "note": row.note,
            })
        })
        .collect()
}

/// The `{id, disabled}` a set takes.
fn set_row(params: &Value) -> Result<(&str, bool), RpcError> {
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::BadParams("set takes an id string".into()))?;
    let disabled = params
        .get("disabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| RpcError::BadParams("set takes a disabled bool".into()))?;
    Ok((id, disabled))
}

/// The bundle whose patch rows name `id`, listed again after the write:
/// the rows the page shows it under, refreshed.
fn rows_owning(home: &Path, id: &str) -> Result<Vec<BundleRow>, zeron_harness::HarnessError> {
    for bundle in installed_bundles(home)? {
        let rows = bundle_rows(home, &bundle.name)?;
        if rows.iter().any(|row| row.id == id) {
            return Ok(rows);
        }
    }
    Ok(Vec::new())
}

/// The `sdk` profile's bundles, and whether this host can change them at all.
fn list(home: &Path) -> Result<Value, RpcError> {
    let bundles = installed_bundles(home).map_err(|error| RpcError::Failed(error.to_string()))?;
    let bundles: Vec<Value> = bundles
        .into_iter()
        .map(|bundle| {
            json!({
                "name": bundle.name,
                "spec": bundle.spec,
                "active": bundle.active,
                "needsClient": bundle.needs_client,
                "version": bundle.version,
                "enginesDsh": bundle.engines_dsh,
                "description": bundle.description,
            })
        })
        .collect();
    Ok(json!({ "bundles": bundles, "pnpm": pnpm_available() }))
}

/// The operation `method` asks for, from its params.
///
/// Pure, so the validation is testable without a profile or a child process:
/// a spec reaches `dsh plugin`'s argv, so a missing, non-string or invalid one
/// is refused here rather than by a child that fails on its own.
fn plugin_op(method: &str, params: &Value) -> Result<PluginOp, RpcError> {
    let (field, verb) = match method {
        methods::DSH_PLUGIN_ADD => ("spec", "add"),
        methods::DSH_PLUGIN_REMOVE => ("name", "remove"),
        other => return Err(RpcError::UnknownMethod(other.into())),
    };
    let value = params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::BadParams(format!("{verb} takes a {field} string")))?;
    if !valid_spec(value) {
        return Err(RpcError::BadParams(format!(
            "{value:?} is not a plugin spec: an npm name, github:owner/repo, \
             git+https:// URL or absolute path"
        )));
    }
    Ok(match method {
        methods::DSH_PLUGIN_ADD => PluginOp::Add(value.to_owned()),
        _ => PluginOp::Remove(value.to_owned()),
    })
}

/// The package pnpm refused to build, when the run says there is one: a
/// failed run whose log carries pnpm's hint ("Ignored build scripts", or
/// the "pnpm approve-builds" it points to) names the first package it
/// lists — several are comma separated. The name is kept whole, a scoped
/// one's '/' included, cut at the '@' that starts the version or resolved
/// id following it.
fn build_approval(run: &PluginRun) -> Option<String> {
    if run.ok
        || !(run.log_tail.contains("Ignored build scripts")
            || run.log_tail.contains("approve-builds"))
    {
        return None;
    }
    let list = run.log_tail.split_once("Ignored build scripts:")?.1;
    let first = list
        .split([',', '\n'])
        .next()?
        .trim()
        .trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '│');
    let end = first
        .char_indices()
        .find(|(i, c)| *i > 0 && *c == '@')
        .map_or(first.len(), |(i, _)| i);
    let name = &first[..end];
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_five_plugin_methods_are_handled_and_providers_is_not() {
        for method in [
            methods::DSH_PLUGINS,
            methods::DSH_PLUGIN_ADD,
            methods::DSH_PLUGIN_REMOVE,
            methods::DSH_PLUGIN_ROWS,
            methods::DSH_PLUGIN_SET_ROW,
        ] {
            assert!(handles(method), "{method}");
        }
        assert!(!handles(methods::DSH_PROVIDERS));
    }

    #[test]
    fn set_row_needs_an_id_and_a_bool() {
        for params in [
            json!({}),
            json!({ "id": "tool-fs" }),
            json!({ "disabled": true }),
            json!({ "id": 7, "disabled": true }),
            json!({ "id": "tool-fs", "disabled": "yes" }),
        ] {
            assert!(
                matches!(set_row(&params), Err(RpcError::BadParams(_))),
                "{params}"
            );
        }
        assert_eq!(
            set_row(&json!({ "id": "tool-fs", "disabled": true })).unwrap(),
            ("tool-fs", true)
        );
    }

    #[test]
    fn add_needs_a_valid_spec() {
        for params in [
            json!({}),
            json!({ "spec": 7 }),
            json!({ "spec": null }),
            json!({ "spec": "-rf" }),
            json!({ "spec": "a b" }),
        ] {
            assert!(
                matches!(
                    plugin_op(methods::DSH_PLUGIN_ADD, &params),
                    Err(RpcError::BadParams(_))
                ),
                "{params}"
            );
        }
        assert_eq!(
            plugin_op(methods::DSH_PLUGIN_ADD, &json!({ "spec": "dsh-hello-plugin" })).unwrap(),
            PluginOp::Add("dsh-hello-plugin".into())
        );
    }

    #[test]
    fn remove_needs_a_valid_name() {
        for params in [
            json!({}),
            json!({ "name": 7 }),
            json!({ "name": null }),
            json!({ "name": "-rf" }),
            json!({ "name": "a b" }),
        ] {
            assert!(
                matches!(
                    plugin_op(methods::DSH_PLUGIN_REMOVE, &params),
                    Err(RpcError::BadParams(_))
                ),
                "{params}"
            );
        }
        assert_eq!(
            plugin_op(methods::DSH_PLUGIN_REMOVE, &json!({ "name": "@scope/pkg" })).unwrap(),
            PluginOp::Remove("@scope/pkg".into())
        );
    }

    fn run(ok: bool, log: &str) -> PluginRun {
        PluginRun {
            ok,
            exit_code: Some(if ok { 0 } else { 1 }),
            log_tail: log.into(),
        }
    }

    #[test]
    fn a_failed_run_with_pnpms_hint_names_the_first_ignored_build() {
        for (log, want) in [
            // strict-dep-builds' error form, inside pnpm's box.
            (
                "  │   Ignored build scripts: esbuild@0.28.2.   │\n\
                 Run \"pnpm approve-builds\" to pick which dependencies should be allowed to run scripts.",
                Some("esbuild"),
            ),
            // Several names are comma separated: the first names it.
            (
                "Ignored build scripts: @scope/dsh-hello-plugin@1.0.0, esbuild@0.28.2.",
                Some("@scope/dsh-hello-plugin"),
            ),
            // A scoped name keeps its '/' when nothing but the scope is '@'.
            (
                "Ignored build scripts: @parcel/watcher, esbuild.",
                Some("@parcel/watcher"),
            ),
            // A git-hosted one is listed under its resolved id: the name
            // alone is what a `package:` row can hold.
            (
                "Ignored build scripts: dsh-hello-plugin@git+https://github.com/o/r.git#61428e2f.",
                Some("dsh-hello-plugin"),
            ),
            // The hint alone names no package.
            (
                "Run \"pnpm approve-builds\" to pick which dependencies should be allowed to run scripts.",
                None,
            ),
            ("added 1 package in 2s", None),
        ] {
            assert_eq!(
                build_approval(&run(false, log)).as_deref(),
                want,
                "{log:?}"
            );
        }
        // A run that landed never asks, even when its log carries the hint.
        assert_eq!(
            build_approval(&run(true, "Ignored build scripts: esbuild@0.28.2.")),
            None
        );
    }
}
