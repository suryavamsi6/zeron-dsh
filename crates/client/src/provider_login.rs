//! Compatibility surface for retired browser account sign-ins.

use crate::{Client, error::{ClientError, Result}};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginProvider {
    Claude,
    Codex,
}

#[derive(Debug, Clone)]
pub struct ProviderLoginStart {
    pub login_id: String,
    pub url: String,
}

impl Client {
    pub async fn provider_login_start(&self, _provider: LoginProvider, _done_url: &str) -> Result<ProviderLoginStart> {
        Err(ClientError::Unsupported("Browser provider sign-in is no longer supported. Configure a DeepSeek API key on the run device.".into()))
    }

    pub async fn provider_login_finish(&self, _login_id: &str) -> Result<()> {
        Err(ClientError::Unsupported("Legacy provider sign-in is unsupported.".into()))
    }

    pub fn provider_login_cancel(&self, _login_id: &str) {}
}
