//! DSH's provider, credential and account surface, over its own app gateway.
//!
//! DSH Web drives this surface through `settings/*`, `credentials/*`, `llm/*`
//! and `account/*`; Zeron asks the same controllers the same questions so a
//! provider configured here is the provider the runtime uses, with no second
//! store. The gateway, not this module, is the source of truth: every call
//! forwards one of the descriptors the installed harness declares, and a
//! method outside that table is refused rather than guessed at.

use serde_json::{json, Value};
use zeron_rpc::{methods, RpcError};

/// Whether `method` is answered from DSH's gateway.
pub fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::DSH_PROVIDERS
            | methods::DSH_SETTINGS
            | methods::DSH_MUTATE_SETTINGS
            | methods::DSH_CREDENTIALS
            | methods::DSH_SET_CREDENTIAL
            | methods::DSH_UNSET_CREDENTIAL
            | methods::DSH_ACCOUNT
            | methods::DSH_SIGN_IN
            | methods::DSH_CANCEL_SIGN_IN
            | methods::DSH_SIGN_OUT
            | methods::DSH_DISCOVER_MODELS
            | methods::DSH_SESSIONS
            | methods::DSH_PROJECTIONS
            | methods::DSH_CHATGPT_START
            | methods::DSH_CHATGPT_CANCEL
            | methods::DSH_CHATGPT_STATUS
            | methods::DSH_CHATGPT_SELECT
            | methods::DSH_CHATGPT_ACCOUNTS
            | methods::DSH_CHATGPT_REMOVE
            | methods::DSH_CHATGPT_USE_RESET
            | methods::DSH_PROVIDER_AUTH_STATUS
            | methods::DSH_PROVIDER_AUTH_START
            | methods::DSH_PROVIDER_AUTH_CANCEL
            | methods::DSH_PROVIDER_AUTH_RESPOND
            | methods::DSH_PROVIDER_AUTH_REMOVE
    )
}

/// The gateway method and argument object one Zeron call forwards to.
///
/// `origin` is the gateway's own address: a sign-in callback comes back to the
/// service that started it, exactly as DSH Web's does.
pub fn translate(
    method: &str,
    params: &Value,
    origin: &str,
) -> Result<(&'static str, Value), RpcError> {
    let object = params.as_object().ok_or_else(|| {
        RpcError::BadParams("Provider settings take an object of arguments".into())
    })?;
    let field = |name: &str| object.get(name).cloned();
    let required = |name: &str| {
        field(name).ok_or_else(|| RpcError::BadParams(format!("{name} is required")))
    };
    let provider = || -> Result<Value, RpcError> {
        match field("provider") {
            None => Ok(json!("openai")),
            Some(Value::String(value)) if value == "openai" || value == "openai-codex" => Ok(json!(value)),
            _ => Err(RpcError::BadParams("Unknown ChatGPT provider".into())),
        }
    };
    let oauth_provider = || -> Result<Value, RpcError> {
        match field("provider") {
            Some(Value::String(value))
                if !value.is_empty()
                    && value.len() <= 64
                    && value.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                    && value.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
                    && value
                        .bytes()
                        .enumerate()
                        .all(|(_, byte)| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') =>
            {
                Ok(json!(value))
            }
            _ => Err(RpcError::BadParams("Unknown provider".into())),
        }
    };
    let translated = match method {
        methods::DSH_PROVIDERS => ("llm/listConfigurableProviders", json!({})),
        methods::DSH_SETTINGS => ("settings/describe", json!({})),
        methods::DSH_MUTATE_SETTINGS => {
            let mut args = json!({ "ns": required("ns")?, "ops": required("ops")? });
            // An absent revision is absent, not null: the gateway validates the
            // argument list, and a null would be a value it must reject.
            if let Some(revision) = field("expectedRevision").filter(|value| !value.is_null()) {
                args["expectedRevision"] = revision;
            }
            ("settings/mutate", args)
        }
        methods::DSH_CREDENTIALS => ("credentials/describe", json!({ "refs": required("refs")? })),
        methods::DSH_SET_CREDENTIAL => (
            "credentials/set",
            json!({ "ref": required("ref")?, "value": required("value")? }),
        ),
        methods::DSH_UNSET_CREDENTIAL => {
            ("credentials/unset", json!({ "ref": required("ref")? }))
        }
        methods::DSH_ACCOUNT => ("account/getState", json!({})),
        methods::DSH_SIGN_IN => (
            "account/startSignIn",
            json!({
                "client": client(params),
                "callbackOrigin": origin,
                "loginSource": "desktop",
            }),
        ),
        methods::DSH_CANCEL_SIGN_IN => (
            "account/cancelSignIn",
            json!({ "attemptId": required("attemptId")? }),
        ),
        methods::DSH_SIGN_OUT => ("account/signOut", json!({ "client": client(params) })),
        methods::DSH_DISCOVER_MODELS => (
            "llm/discoverModels",
            json!({
                "settingsNs": required("settingsNs")?,
                "request": field("request").unwrap_or_else(|| json!({})),
            }),
        ),
        // Zeron's own plugin, loaded into the same gateway: DSH's login path
        // refuses this provider, so these are Zeron's surface.
        methods::DSH_CHATGPT_START => ("zeron-chatgpt/start", json!({ "provider": provider()? })),
        methods::DSH_CHATGPT_CANCEL => ("zeron-chatgpt/cancel", json!({ "provider": provider()? })),
        methods::DSH_CHATGPT_SELECT => ("zeron-chatgpt/select", json!({ "accountId": required("accountId")?, "provider": provider()? })),
        methods::DSH_CHATGPT_REMOVE => ("zeron-chatgpt/remove", json!({ "accountId": required("accountId")?, "provider": provider()? })),
        methods::DSH_CHATGPT_ACCOUNTS => ("zeron-chatgpt/accounts", json!({ "provider": provider()? })),
        methods::DSH_CHATGPT_USE_RESET => ("zeron-chatgpt/useReset", json!({
            "accountId": required("accountId")?, "creditId": required("creditId")?,
            "idempotencyKey": required("idempotencyKey")?, "provider": provider()?,
        })),
        methods::DSH_CHATGPT_STATUS => ("zeron-chatgpt/status", json!({ "provider": provider()? })),
        methods::DSH_PROVIDER_AUTH_STATUS => (
            "zeron-provider-auth/status",
            json!({ "provider": oauth_provider()? }),
        ),
        methods::DSH_PROVIDER_AUTH_SELECT => (
            "zeron-provider-auth/select",
            json!({ "provider": oauth_provider()?, "accountId": required("accountId")? }),
        ),
        methods::DSH_PROVIDER_AUTH_REMOVE => (
            "zeron-provider-auth/remove",
            json!({ "provider": oauth_provider()?, "accountId": required("accountId")? }),
        ),
        methods::DSH_PROVIDER_AUTH_START => {
            let mut args = json!({ "provider": oauth_provider()? });
            if let Some(method) = field("method").filter(Value::is_string) {
                args["method"] = method;
            }
            ("zeron-provider-auth/start", args)
        }
        methods::DSH_PROVIDER_AUTH_CANCEL => (
            "zeron-provider-auth/cancel",
            json!({ "provider": oauth_provider()? }),
        ),
        methods::DSH_PROVIDER_AUTH_RESPOND => (
            "zeron-provider-auth/respond",
            json!({
                "provider": oauth_provider()?,
                "promptId": required("promptId")?,
                "value": required("value")?,
            }),
        ),
        // The meter's Remotes take their request under a named argument, which
        // the gateway validates by name: `_request` for the list, `request` for
        // one session's projections.
        methods::DSH_SESSIONS => ("session/list", json!({ "_request": {} })),
        methods::DSH_PROJECTIONS => (
            "session/projections",
            json!({ "request": { "sessionId": required("sessionId")? } }),
        ),
        other => {
            return Err(RpcError::Failed(format!(
                "{other} is not part of the DeepSeek Harness provider surface"
            )));
        }
    };
    Ok(translated)
}

/// The account calls' client identity: the app, its language and its offset.
/// DSH uses them for the Platform's wire locale and relative times.
fn client(params: &Value) -> Value {
    json!({
        "version": params.get("version").and_then(Value::as_str)
            .unwrap_or(env!("CARGO_PKG_VERSION")),
        "locale": params.get("locale").and_then(Value::as_str).unwrap_or("en"),
        "timezoneOffsetSeconds": params.get("timezoneOffsetSeconds")
            .and_then(Value::as_i64).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_call_reads_its_own_arguments() {
        let origin = "http://127.0.0.1:49152";
        let (call, args) = translate(
            methods::DSH_SET_CREDENTIAL,
            &json!({ "ref": "llm-pi-ai/openrouter", "value": { "key": "oc_sk_x" } }),
            origin,
        )
        .unwrap();
        assert_eq!(call, "credentials/set");
        assert_eq!(args["ref"], "llm-pi-ai/openrouter");
        assert_eq!(args["value"]["key"], "oc_sk_x");

        let (call, args) = translate(
            methods::DSH_MUTATE_SETTINGS,
            &json!({ "ns": "llm-pi-ai", "ops": [{ "op": "delete", "path": ["providers", "x"] }] }),
            origin,
        )
        .unwrap();
        assert_eq!(call, "settings/mutate");
        assert_eq!(args["ns"], "llm-pi-ai");
        assert!(
            args.get("expectedRevision").is_none(),
            "an absent revision stays absent: the gateway validates the argument list, so a null would be refused"
        );

        let (_, args) = translate(
            methods::DSH_MUTATE_SETTINGS,
            &json!({ "ns": "llm-pi-ai", "ops": [], "expectedRevision": 4 }),
            origin,
        )
        .unwrap();
        assert_eq!(args["expectedRevision"], 4, "a caller that read a revision sends it");
    }

    #[test]
    fn the_token_meter_is_read_through_the_argument_names_its_remotes_declare() {
        let origin = "http://127.0.0.1:49152";
        assert!(handles(methods::DSH_SESSIONS));
        assert!(handles(methods::DSH_PROJECTIONS));
        let (call, args) = translate(methods::DSH_SESSIONS, &json!({}), origin).unwrap();
        assert_eq!(call, "session/list");
        assert_eq!(args, json!({ "_request": {} }), "the list Remote names its request `_request`");
        let (call, args) = translate(
            methods::DSH_PROJECTIONS,
            &json!({ "sessionId": "ses_1" }),
            origin,
        )
        .unwrap();
        assert_eq!(call, "session/projections");
        assert_eq!(args, json!({ "request": { "sessionId": "ses_1" } }));
        assert!(
            translate(methods::DSH_PROJECTIONS, &json!({}), origin).is_err(),
            "a projection read without a session is refused, not defaulted"
        );
    }

    #[test]
    fn a_sign_in_callback_comes_back_to_the_service_that_started_it() {
        let origin = "http://127.0.0.1:49152";
        let (call, args) = translate(
            methods::DSH_SIGN_IN,
            &json!({ "locale": "hi", "timezoneOffsetSeconds": 19800 }),
            origin,
        )
        .unwrap();
        assert_eq!(call, "account/startSignIn");
        assert_eq!(args["callbackOrigin"], origin);
        assert_eq!(args["loginSource"], "desktop");
        assert_eq!(args["client"]["locale"], "hi");
        assert_eq!(args["client"]["timezoneOffsetSeconds"], 19800);
        assert_eq!(args["client"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_chatgpt_sign_in_is_zerons_own_plugin_on_the_same_gateway() {
        let origin = "http://127.0.0.1:49152";
        for (method, call) in [
            (methods::DSH_CHATGPT_START, "zeron-chatgpt/start"),
            (methods::DSH_CHATGPT_CANCEL, "zeron-chatgpt/cancel"),
            (methods::DSH_CHATGPT_STATUS, "zeron-chatgpt/status"),
            (methods::DSH_CHATGPT_ACCOUNTS, "zeron-chatgpt/accounts"),
        ] {
            assert!(handles(method), "{method} is part of the surface");
            let (translated, args) = translate(method, &json!({}), origin).unwrap();
            assert_eq!(translated, call);
            assert_eq!(args, json!({"provider": "openai"}), "the default provider is explicit");
        }
    }

    #[test]
    fn chatgpt_account_writes_forward_the_named_arguments() {
        let params = json!({"accountId": "account", "creditId": "credit", "idempotencyKey": "request"});
        let (call, args) = translate(methods::DSH_CHATGPT_SELECT, &params, "x").unwrap();
        assert_eq!(call, "zeron-chatgpt/select");
        assert_eq!(args, json!({"accountId": "account", "provider": "openai"}));
        let (call, args) = translate(methods::DSH_CHATGPT_USE_RESET, &params, "x").unwrap();
        assert_eq!(call, "zeron-chatgpt/useReset");
        assert_eq!(args, json!({"accountId": "account", "creditId": "credit", "idempotencyKey": "request", "provider": "openai"}));
        let (_, codex) = translate(methods::DSH_CHATGPT_ACCOUNTS, &json!({"provider": "openai-codex"}), "x").unwrap();
        assert_eq!(codex["provider"], "openai-codex");
        assert!(translate(methods::DSH_CHATGPT_START, &json!({"provider": "other"}), "x").is_err());
        assert!(translate(methods::DSH_CHATGPT_USE_RESET, &json!({}), "x").is_err());
    }

    #[test]
    fn a_missing_argument_is_named_and_nothing_is_invented() {
        let error = translate(
            methods::DSH_CANCEL_SIGN_IN,
            &json!({}),
            "http://127.0.0.1:1",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("attemptId"), "{error}");
        assert!(translate(methods::DSH_CREDENTIALS, &json!({}), "x").is_err());
    }

    #[test]
    fn only_the_declared_surface_is_answered() {
        assert!(handles(methods::DSH_PROVIDERS));
        assert!(!handles(methods::LIST_HARNESSES));
        assert!(
            translate("settings/replace", &json!({}), "x").is_err(),
            "a redacted replace silently drops secrets; it is not offered"
        );
    }
}
