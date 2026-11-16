//! DeepSeek setup on a run device. Historical account data stays on its host.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use zeron_proto::{AgentAccountsSnapshot, AgentLoginPoll, AgentLoginStart};

use crate::{Client, catalog::HarnessInfo, client::Backend, error::{ClientError, Result}, lock};

fn unsupported_login() -> ClientError {
    ClientError::Unsupported("Browser sign-in is unsupported. Configure a DeepSeek API key on the run device.".into())
}

impl Client {
    pub async fn device_login_start(&self, _device_id: &str, _harness: &str, _provider: Option<&str>, _done_url: &str) -> Result<AgentLoginStart> {
        Err(unsupported_login())
    }

    pub async fn device_login_poll(&self, _device_id: &str, _login_id: &str, _done_url: &str) -> Result<AgentLoginPoll> {
        Err(unsupported_login())
    }

    pub async fn device_login_complete(&self, _device_id: &str, _login_id: &str, _code: &str) -> Result<AgentAccountsSnapshot> {
        Err(unsupported_login())
    }

    pub fn device_login_cancel(&self, _device_id: &str, _login_id: &str) {}

    pub async fn set_harness_enabled(&self, device_id: &str, harness: &str, enabled: bool) -> Result<Vec<HarnessInfo>> {
        if harness != "deepseek" {
            return Err(ClientError::Unsupported("Only DeepSeek is supported. Historical settings were not changed.".into()));
        }
        let list = match self.inner.backend() {
            Backend::Demo(demo) => {
                lock(&DEMO_DISABLED).insert(harness.to_owned(), !enabled);
                demo.list_harnesses(device_id).await
            }
            Backend::Live(live) => {
                let reply = live.relay.call(device_id, zeron_rpc::methods::SET_HARNESS_ENABLED,
                    serde_json::json!({ "harness": harness, "enabled": enabled })).await?;
                serde_json::from_value(reply).map_err(|e| ClientError::HostError(e.to_string()))?
            }
        };
        let list = with_demo_toggles(self.inner.is_demo(), list);
        lock(&self.inner.harness_catalogs).insert(device_id.to_owned(), list.clone());
        self.inner.recompute_workspace();
        Ok(list)
    }

    pub async fn device_agent_accounts(&self, device_id: &str) -> Result<AgentAccountsSnapshot> {
        self.accounts_call(device_id, zeron_rpc::methods::LIST_AGENT_ACCOUNTS, serde_json::json!({})).await
    }

    pub async fn device_add_api_key(&self, device_id: &str, key: &str) -> Result<AgentAccountsSnapshot> {
        if key.trim().is_empty() {
            return Err(ClientError::InvalidArgument("Enter a DeepSeek API key first.".into()));
        }
        self.accounts_call(device_id, zeron_rpc::methods::ADD_AGENT_API_KEY,
            serde_json::json!({ "harness": "deepseek", "provider": "deepseek", "key": key.trim() })).await
    }

    pub async fn device_activate_account(&self, _device_id: &str, _harness: &str, _account_id: &str) -> Result<AgentAccountsSnapshot> {
        Err(ClientError::Unsupported("Legacy account switching is unsupported. Configure DeepSeek explicitly.".into()))
    }

    pub async fn device_forget_account(&self, _device_id: &str, _harness: &str, _account_id: &str) -> Result<AgentAccountsSnapshot> {
        Err(ClientError::Unsupported("Historical provider credentials are preserved; manage them outside Zeron.".into()))
    }

    async fn accounts_call(&self, device_id: &str, method: &str, params: serde_json::Value) -> Result<AgentAccountsSnapshot> {
        match self.inner.backend() {
            Backend::Demo(_) => Ok(AgentAccountsSnapshot { accounts: Vec::new(), warnings: Vec::new() }),
            Backend::Live(live) => {
                let reply = live.relay.call(device_id, method, params).await?;
                serde_json::from_value(reply).map_err(|e| ClientError::HostError(e.to_string()))
            }
        }
    }
}

static DEMO_DISABLED: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(Default::default);

pub(crate) fn with_demo_toggles(demo: bool, mut list: Vec<HarnessInfo>) -> Vec<HarnessInfo> {
    list.retain(|h| h.id == "deepseek");
    if demo {
        let disabled = lock(&DEMO_DISABLED);
        for harness in &mut list {
            if disabled.get(&harness.id).copied().unwrap_or(false) {
                harness.enabled = Some(false);
            }
        }
    }
    list
}
