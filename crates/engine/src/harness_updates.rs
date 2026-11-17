//! DeepSeek SDK installations are managed by their installer, not legacy agent updaters.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use zeron_proto::{
    HarnessId, HarnessInstallSource, HarnessUpdateFailure, HarnessUpdateFailureKind,
    HarnessUpdatePhase, HarnessUpdatePolicy, HarnessUpdateStatus,
};

use crate::registry::HarnessRegistry;

pub(crate) const APPLY_REPLY_DEADLINE: Duration = Duration::from_secs(60);
/// No legacy installer runs here any more: the SDK is installed by its own
/// installer. Retained (and deliberately shorter than [`APPLY_REPLY_DEADLINE`])
/// because the RPC layer bounds its wait for another device's update answer by
/// this constant.
pub(crate) const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);
const MANAGED_BY_SDK: &str = "Install or update the official deepseek-ai/deepseek-harness SDK with its documented installer.";

#[derive(Clone)]
pub struct HarnessUpdateCoordinator {
    statuses: watch::Sender<Vec<HarnessUpdateStatus>>,
}

fn status(harness: HarnessId, policy: HarnessUpdatePolicy) -> HarnessUpdateStatus {
    let supported = harness == HarnessId::DeepSeek;
    HarnessUpdateStatus {
        harness,
        installed_version: None,
        latest_version: None,
        channel: None,
        source: HarnessInstallSource::Unknown,
        policy,
        phase: if supported {
            HarnessUpdatePhase::ManualActionRequired
        } else {
            HarnessUpdatePhase::Failed
        },
        progress: None,
        checked_at: None,
        error: (!supported).then(|| HarnessUpdateFailure {
            message: "This legacy provider is no longer supported; select DeepSeek.".into(),
            retryable: false,
            kind: HarnessUpdateFailureKind::Check,
        }),
        can_apply: false,
        manual_command: None,
        wait_reason: supported.then(|| MANAGED_BY_SDK.into()),
        compatibility: None,
        held_back: None,
    }
}

impl HarnessUpdateCoordinator {
    pub fn new(_data_dir: &Path, _registry: Arc<HarnessRegistry>) -> Self {
        Self {
            statuses: watch::channel(vec![status(
                HarnessId::DeepSeek,
                HarnessUpdatePolicy::Notify,
            )])
            .0,
        }
    }

    pub fn start(&self) {}
    pub fn recheck_changed_binaries_soon(&self) {}
    pub async fn recheck_changed_binaries(&self) {}
    pub fn track(&self, _harness: HarnessId) {}
    pub fn untrack(&self, _harness: HarnessId) {}
    pub fn set_compatibility_feed(&self, _url: String) {}
    pub fn refresh_enabled(&self) {}

    pub fn watch(&self) -> watch::Receiver<Vec<HarnessUpdateStatus>> {
        self.statuses.subscribe()
    }

    pub fn snapshot(&self) -> Vec<HarnessUpdateStatus> {
        self.statuses.borrow().clone()
    }

    pub async fn check_all(&self) -> Vec<HarnessUpdateStatus> {
        self.snapshot()
    }

    pub async fn check_one(&self, harness: HarnessId) -> Result<(), String> {
        if harness != HarnessId::DeepSeek {
            return Err("This legacy provider is no longer supported; select DeepSeek.".into());
        }
        Ok(())
    }

    pub async fn apply(&self, harness: HarnessId) -> Result<String, String> {
        self.check_one(harness).await?;
        Err(MANAGED_BY_SDK.into())
    }

    pub fn cancel(&self, _harness: HarnessId) -> bool {
        false
    }

    pub fn dismiss(&self, harness: HarnessId, _version: Option<String>) -> HarnessUpdateStatus {
        status(harness, HarnessUpdatePolicy::Notify)
    }

    pub fn set_policy(
        &self,
        harness: HarnessId,
        policy: HarnessUpdatePolicy,
    ) -> HarnessUpdateStatus {
        let updated = status(harness, policy);
        if harness == HarnessId::DeepSeek {
            self.statuses.send_replace(vec![updated.clone()]);
        }
        updated
    }

    pub async fn shutdown(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_update_requests_have_no_installer_or_retry_path() {
        let old = status(HarnessId::Codex, HarnessUpdatePolicy::AutoWhenIdle);
        assert_eq!(old.phase, HarnessUpdatePhase::Failed);
        assert!(!old.can_apply);
        assert!(old.manual_command.is_none());
        assert!(!old.error.unwrap().retryable);
        let sdk = status(HarnessId::DeepSeek, HarnessUpdatePolicy::Notify);
        assert!(!sdk.can_apply);
        assert_eq!(sdk.phase, HarnessUpdatePhase::ManualActionRequired);
    }
}
