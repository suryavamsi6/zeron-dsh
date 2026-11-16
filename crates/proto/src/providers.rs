use crate::HarnessId;

pub const PROVIDERS: [(HarnessId, &str, &str); 1] =
    [(HarnessId::DeepSeek, "DeepSeek", "dsh")];

pub fn signs_in(harness: HarnessId) -> bool {
    harness == HarnessId::DeepSeek
}

pub fn provider_name(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::DeepSeek => "DeepSeek",
        HarnessId::Mock => "Mock",
        _ => "Unsupported legacy provider",
    }
}

pub fn reports_usage(_harness: HarnessId) -> bool {
    false
}

pub fn keeps_one_login(harness: HarnessId) -> bool {
    harness == HarnessId::DeepSeek
}

pub fn switches_accounts(_harness: HarnessId) -> bool {
    false
}

pub fn provider_note(harness: HarnessId) -> Option<&'static str> {
    (harness == HarnessId::DeepSeek).then_some(
        "DeepSeek runs through the official Harness JSON-RPC SDK in Zeron's isolated Harness home.",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginOption {
    pub provider: Option<&'static str>,
    pub label: &'static str,
}

pub fn login_options(_harness: HarnessId) -> Vec<LoginOption> {
    Vec::new()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiKeyOption {
    pub provider: &'static str,
    pub label: &'static str,
}

pub fn api_key_options(harness: HarnessId) -> Vec<ApiKeyOption> {
    if harness == HarnessId::DeepSeek {
        vec![ApiKeyOption { provider: "deepseek", label: "DeepSeek" }]
    } else {
        Vec::new()
    }
}

pub fn api_key_option(harness: HarnessId, provider: &str) -> Option<ApiKeyOption> {
    api_key_options(harness).into_iter().find(|option| option.provider == provider)
}

pub fn add_key_label(option: ApiKeyOption, in_group: bool) -> String {
    if in_group {
        "Add API key".to_string()
    } else {
        format!("Add {} API key", option.label)
    }
}

pub fn groups_by_provider(_harness: HarnessId) -> bool {
    false
}

pub fn login_option(_harness: HarnessId, _provider: &str) -> Option<LoginOption> {
    None
}

pub fn account_group_label(harness: HarnessId, provider: &str) -> String {
    if harness == HarnessId::DeepSeek && matches!(provider, "deepseek" | "deepseek-official") {
        "DeepSeek".to_string()
    } else {
        "Unsupported legacy provider".to_string()
    }
}

pub fn account_group_hint(harness: HarnessId, _provider: &str) -> Option<String> {
    provider_note(harness).map(str::to_owned)
}

pub fn add_account_label(harness: HarnessId, _empty: bool) -> String {
    if harness == HarnessId::DeepSeek {
        "Add DeepSeek API key".to_string()
    } else {
        "Unsupported legacy provider".to_string()
    }
}

pub fn add_option_label(harness: HarnessId, _option: LoginOption, empty: bool) -> String {
    add_account_label(harness, empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_deepseek_has_provider_settings() {
        assert_eq!(PROVIDERS, [(HarnessId::DeepSeek, "DeepSeek", "dsh")]);
        assert_eq!(api_key_options(HarnessId::DeepSeek).len(), 1);
        assert!(login_options(HarnessId::DeepSeek).is_empty());
        for slug in ["codex", "claude-code", "pi", "opencode", "hermes", "acp.gemini"] {
            let legacy = HarnessId::from_slug(slug).unwrap();
            assert!(!legacy.is_supported());
            assert!(!signs_in(legacy));
            assert!(api_key_options(legacy).is_empty());
            assert!(login_options(legacy).is_empty());
        }
    }
}
