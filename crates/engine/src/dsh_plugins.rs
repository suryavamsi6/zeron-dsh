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
        engines_refusal, installed_bundles, pnpm_available, run_plugin_op, valid_spec, PluginOp,
    },
    DeepSeekHarness,
};
use zeron_rpc::{methods, RpcError};

/// One `dsh plugin` run at a time: pnpm writes the whole profile, so a second
/// install started while the first is running would corrupt it.
static INSTALL: Mutex<()> = Mutex::const_new(());

/// Whether `method` is answered here.
pub fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::DSH_PLUGINS | methods::DSH_PLUGIN_ADD | methods::DSH_PLUGIN_REMOVE
    )
}

/// Answer one of the three methods.
pub async fn call(
    harness: &DeepSeekHarness,
    method: &str,
    params: &Value,
) -> Result<Value, RpcError> {
    match method {
        methods::DSH_PLUGINS => list(harness.home()),
        methods::DSH_PLUGIN_ADD | methods::DSH_PLUGIN_REMOVE => {
            let op = plugin_op(method, params)?;
            // Held across the run, not just the spawn: the profile is one
            // pnpm project and the child owns it until it exits.
            let _running = INSTALL.lock().await;
            let run = run_plugin_op(harness.sdk_root(), harness.home(), op)
                .await
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            Ok(json!({
                "ok": run.ok,
                "exitCode": run.exit_code,
                "log": run.log_tail,
                "refusal": engines_refusal(&run.log_tail),
            }))
        }
        other => Err(RpcError::UnknownMethod(other.into())),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_plugin_methods_are_handled_and_providers_is_not() {
        for method in [
            methods::DSH_PLUGINS,
            methods::DSH_PLUGIN_ADD,
            methods::DSH_PLUGIN_REMOVE,
        ] {
            assert!(handles(method), "{method}");
        }
        assert!(!handles(methods::DSH_PROVIDERS));
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
}
