//! Legacy ACP configuration requests are rejected without touching saved agent records.

use crate::registry::HarnessRegistry;
use serde_json::Value;
use std::path::Path;
use zeron_rpc::{RpcError, RpcReply, methods};

pub fn start(_data_dir: &Path, _registry: &HarnessRegistry) {}

pub(crate) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_ACP_REGISTRY
            | methods::ADD_ACP_AGENT
            | methods::REMOVE_ACP_AGENT
            | methods::GET_ACP_AGENT_ENV
            | methods::SET_ACP_AGENT_ENV
            | methods::GET_ACP_AGENT_SIGN_IN
            | methods::SIGN_OUT_ACP_AGENT
            | methods::SET_ACP_AGENT_KEY
            | methods::ADD_OWN_AGENT
    )
}

pub(crate) async fn dispatch(
    _registry: &HarnessRegistry,
    _updates: Option<&crate::harness_updates::HarnessUpdateCoordinator>,
    _method: &str,
    _params: Value,
) -> Result<RpcReply, RpcError> {
    Err(RpcError::Failed("Custom and ACP agents are no longer supported; select DeepSeek. Saved agent records have not been changed.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_configuration_methods_remain_explicitly_recognized() {
        for method in [
            methods::ADD_ACP_AGENT,
            methods::ADD_OWN_AGENT,
            methods::SET_ACP_AGENT_KEY,
        ] {
            assert!(handles(method));
        }
    }
}
