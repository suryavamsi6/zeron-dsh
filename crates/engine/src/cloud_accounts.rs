//! DeepSeek API-key accounts for Cloud; legacy provider requests never mutate the vault.

use crate::agent_accounts::AgentAccounts;
use crate::cloud_client::CloudClient;
use serde::Deserialize;
use zeron_proto::{
    AgentAccount, AgentAccountsSnapshot, AgentAuthKind, CLOUD_ACCOUNTS_DEVICE, HarnessId,
    VaultAccount, VaultConnectionStatus, VaultProvider,
};
use zeron_rpc::{RpcError, RpcReply, methods, parse_params};

pub(crate) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_AGENT_ACCOUNTS
            | methods::ACTIVATE_AGENT_ACCOUNT
            | methods::FORGET_AGENT_ACCOUNT
            | methods::START_AGENT_LOGIN
            | methods::COMPLETE_AGENT_LOGIN
            | methods::POLL_AGENT_LOGIN
            | methods::CANCEL_AGENT_LOGIN
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountParams {
    harness: HarnessId,
    account_id: String,
}

fn account_ref(harness: HarnessId, account_id: &str) -> Result<&str, RpcError> {
    if harness != HarnessId::DeepSeek {
        return Err(RpcError::Failed(
            "This legacy provider is no longer supported; select DeepSeek.".into(),
        ));
    }
    account_id
        .strip_prefix("deepseek-key:")
        .filter(|slot| {
            !slot.is_empty()
                && slot.len() <= 128
                && slot
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .ok_or_else(|| RpcError::BadParams("Expected a DeepSeek Cloud account id.".into()))
}

pub(crate) async fn dispatch(
    cloud: CloudClient,
    _accounts: AgentAccounts,
    method: &str,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let failed = |error: crate::EngineError| RpcError::Failed(error.to_string());
    match method {
        methods::LIST_AGENT_ACCOUNTS => RpcReply::value(&snapshot(&cloud).await?),
        methods::ACTIVATE_AGENT_ACCOUNT => {
            let p: AccountParams = parse_params(params)?;
            let slot = account_ref(p.harness, &p.account_id)?;
            cloud.vault_activate(VaultProvider::DeepseekKey, Some(slot)).await.map_err(failed)?;
            RpcReply::value(&snapshot(&cloud).await?)
        }
        methods::FORGET_AGENT_ACCOUNT => {
            let p: AccountParams = parse_params(params)?;
            let slot = account_ref(p.harness, &p.account_id)?;
            cloud.vault_forget(VaultProvider::DeepseekKey, slot).await.map_err(failed)?;
            RpcReply::value(&snapshot(&cloud).await?)
        }
        _ => Err(RpcError::Failed("DeepSeek uses an API key, not legacy browser sign-in. Add a DeepSeek key for Cloud in Settings.".into())),
    }
}

async fn snapshot(cloud: &CloudClient) -> Result<AgentAccountsSnapshot, RpcError> {
    let accounts = cloud
        .vault_accounts(VaultProvider::DeepseekKey)
        .await
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    Ok(AgentAccountsSnapshot {
        accounts: accounts.iter().map(to_agent_account).collect(),
        warnings: Vec::new(),
    })
}

fn to_agent_account(account: &VaultAccount) -> AgentAccount {
    let reconnect = account.status == VaultConnectionStatus::NeedsReconnect;
    AgentAccount {
        id: format!("deepseek-key:{}", account.slot),
        harness: HarnessId::DeepSeek,
        email: None,
        plan_label: account.account.clone(),
        active: account.active,
        usage_windows: Vec::new(),
        usage_fetched_at: None,
        usage_error: reconnect.then(|| "Replace this DeepSeek API key.".into()),
        codex_reset_credits: None,
        display_name: account.display_name.clone(),
        organization: account.organization.clone(),
        auth_kind: Some(AgentAuthKind::ApiKey),
        switchable: !reconnect,
        saved_at: Some(account.updated_at),
        provider: Some("deepseek-official".into()),
    }
}

pub(crate) async fn ready_harnesses(cloud: &CloudClient) -> Option<Vec<HarnessId>> {
    let vault = cloud.vault_status().await.ok().filter(|v| v.available)?;
    Some(
        if vault.connections.iter().any(|c| {
            c.provider == VaultProvider::DeepseekKey
                && c.status == VaultConnectionStatus::Connected
                && (c.has_active || c.accounts == 0)
        }) {
            vec![HarnessId::DeepSeek]
        } else {
            Vec::new()
        },
    )
}

pub(crate) fn is_cloud_accounts_target(target: &str) -> bool {
    target == CLOUD_ACCOUNTS_DEVICE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_and_cross_provider_account_ids_are_rejected_before_vault_access() {
        assert!(account_ref(HarnessId::Codex, "deepseek-key:slot").is_err());
        assert!(account_ref(HarnessId::DeepSeek, "codex:slot").is_err());
        assert!(account_ref(HarnessId::DeepSeek, "deepseek-key:../slot").is_err());
        assert_eq!(
            account_ref(HarnessId::DeepSeek, "deepseek-key:slot-1").unwrap(),
            "slot-1"
        );
    }
}
