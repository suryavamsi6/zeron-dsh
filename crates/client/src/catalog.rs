//! DeepSeek catalogs and display labels for preserved historical sessions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInfo {
    /// Wire id (`deepseek`).
    pub id: String,
    /// Display name.
    #[serde(alias = "name")]
    pub label: String,
    #[serde(default)]
    pub supports_steering: Option<bool>,
    /// `step-boundary` / `turn-boundary`.
    #[serde(default)]
    pub steering_mode: Option<String>,
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    /// CLI present on the listing device (absent on old engines ⇒ true).
    #[serde(default = "default_true")]
    pub installed: bool,
    /// Offered by the listing device (Settings → Providers). `None` ⇒ legacy.
    #[serde(default)]
    pub enabled: Option<bool>,
}

fn default_true() -> bool {
    true
}

impl HarnessInfo {
    fn fallback(id: &str, label: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            supports_steering: None,
            steering_mode: None,
            reasoning_levels: Vec::new(),
            installed: true,
            enabled: None,
        }
    }

    /// Mid-turn steering is available (known only from a live catalog).
    pub fn mid_turn_steering(&self) -> Option<bool> {
        Some(self.supports_steering? && self.steering_mode.as_deref()? == "step-boundary")
    }

    /// Listed by the device and usable (installed, not disabled).
    pub fn offered(&self) -> bool {
        self.id == "deepseek" && self.installed && self.enabled != Some(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptionChoice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub label: String,
    pub choices: Vec<ModelOptionChoice>,
    pub default_choice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Lowercase wire values (`low`…`ultrathink`); empty = no effort ladder.
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    #[serde(default)]
    pub options: Vec<ModelOption>,
}

fn model(
    id: &str,
    label: &str,
    description: &str,
    ladder: &[&str],
    options: Vec<ModelOption>,
) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        label: label.into(),
        description: Some(description.into()),
        reasoning_levels: ladder.iter().map(|s| (*s).to_owned()).collect(),
        options,
    }
}

pub fn harnesses_without_asking(device_id: &str) -> Option<Vec<HarnessInfo>> {
    zeron_proto::is_cloud_device(device_id, "").then(fallback_harnesses)
}

pub fn fallback_harnesses() -> Vec<HarnessInfo> {
    vec![HarnessInfo::fallback("deepseek", "DeepSeek")]
}

pub fn harness_label(id: &str) -> String {
    if id == "deepseek" {
        return "DeepSeek".into();
    }
    if id == "mock" {
        return "Mock".into();
    }
    format!("{id} (unsupported)")
}

pub fn fallback_models(harness: &str) -> Vec<ModelInfo> {
    if harness != "deepseek" {
        return Vec::new();
    }
    vec![
        model("deepseek-flash", "DeepSeek V4.1 Flash", "DeepSeek Harness default model", &["low", "high", "max"], vec![]),
        model("deepseek-v4-pro", "DeepSeek V4 Pro", "DeepSeek Harness Pro model", &["low", "high", "max"], vec![]),
    ]
}

/// pickers.rs — High when available, then Medium, then the first level.
pub fn default_reasoning(model: &ModelInfo) -> Option<String> {
    let levels = &model.reasoning_levels;
    if levels.is_empty() {
        return None;
    }
    for preferred in ["high", "medium"] {
        if levels.iter().any(|l| l == preferred) {
            return Some(preferred.into());
        }
    }
    levels.first().cloned()
}

pub fn reasoning_label(level: &str) -> String {
    match level {
        "minimal" => "Minimal".into(),
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" => "X-High".into(),
        "max" => "Max".into(),
        "ultra" => "Ultra".into(),
        "ultracode" => "Ultracode".into(),
        "ultrathink" => "Ultrathink".into(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}

pub fn normalize_models(harness: &str, models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    if harness != "deepseek" {
        return Vec::new();
    }
    let has_real = models.iter().any(|m| !m.id.eq_ignore_ascii_case("default"));
    models.into_iter().filter(|m| !has_real || !m.id.eq_ignore_ascii_case("default")).collect()
}

static LEARNED_LABELS: std::sync::LazyLock<
    std::sync::RwLock<std::collections::HashMap<(String, String), String>>,
> = std::sync::LazyLock::new(Default::default);

pub(crate) fn learn_labels(harness: &str, models: &[ModelInfo]) {
    let Ok(mut map) = LEARNED_LABELS.write() else { return; };
    for m in models {
        if !m.label.trim().is_empty() && m.label != m.id {
            map.insert((harness.to_owned(), m.id.clone()), m.label.clone());
        }
    }
}

pub fn model_label(harness: &str, model_id: &str) -> String {
    if let Some(found) = fallback_models(harness).iter().find(|m| m.id == model_id) {
        return found.label.clone();
    }
    LEARNED_LABELS.read().ok()
        .and_then(|map| map.get(&(harness.to_owned(), model_id.to_owned())).cloned())
        .unwrap_or_else(|| model_id.to_owned())
}

/// Last-known live catalogs on disk (`{data_dir}/catalogs/{device}.json`):
/// pickers open instantly with the device's real list, and an unreachable
/// device still shows what it offered last time before the static fallback.
#[derive(Debug, Clone)]
pub(crate) struct DiskCatalog {
    dir: std::path::PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DeviceCatalog {
    #[serde(default)]
    harnesses: Option<Vec<HarnessInfo>>,
    #[serde(default)]
    models: std::collections::BTreeMap<String, Vec<ModelInfo>>,
}

fn file_safe(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl DiskCatalog {
    pub(crate) fn new(data_dir: &std::path::Path) -> Self {
        Self {
            dir: data_dir.join("catalogs"),
        }
    }

    fn path(&self, device_id: &str) -> std::path::PathBuf {
        self.dir.join(format!("{}.json", file_safe(device_id)))
    }

    fn load(&self, device_id: &str) -> DeviceCatalog {
        std::fs::read(self.path(device_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn store(&self, device_id: &str, catalog: &DeviceCatalog) {
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let path = self.path(device_id);
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(catalog).unwrap_or_default())?;
            std::fs::rename(tmp, path)
        };
        if let Err(err) = write() {
            tracing::debug!(error = %err, "catalog cache write failed");
        }
    }

    pub(crate) fn harnesses(&self, device_id: &str) -> Option<Vec<HarnessInfo>> {
        self.load(device_id).harnesses
    }

    pub(crate) fn put_harnesses(&self, device_id: &str, list: &[HarnessInfo]) {
        let mut catalog = self.load(device_id);
        catalog.harnesses = Some(list.to_vec());
        self.store(device_id, &catalog);
    }

    pub(crate) fn models(&self, device_id: &str, harness: &str) -> Option<Vec<ModelInfo>> {
        self.load(device_id).models.remove(harness)
    }

    /// Teach [`model_label`] every label cached on disk (client start).
    pub(crate) fn warm_labels(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(catalog) = serde_json::from_slice::<DeviceCatalog>(&bytes) else {
                continue;
            };
            for (harness, models) in &catalog.models {
                learn_labels(harness, models);
            }
        }
    }

    pub(crate) fn put_models(&self, device_id: &str, harness: &str, list: &[ModelInfo]) {
        learn_labels(harness, list);
        let mut catalog = self.load(device_id);
        catalog.models.insert(harness.to_owned(), list.to_vec());
        self.store(device_id, &catalog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_deepseek_is_offered() {
        assert_eq!(fallback_harnesses().iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec!["deepseek"]);
        assert_eq!(harnesses_without_asking("cloud-s-chat-1").unwrap()[0].id, "deepseek");
        assert!(fallback_models("claude-code").is_empty());
        assert!(fallback_models("codex").is_empty());
        assert!(normalize_models("codex", vec![model("old", "Old", "", &[], vec![])]).is_empty());
    }

    #[test]
    fn historical_labels_remain_visible_but_unsupported() {
        assert_eq!(harness_label("codex"), "codex (unsupported)");
        assert_eq!(model_label("codex", "never-cached-historical-id"), "never-cached-historical-id");
    }

    #[test]
    fn disk_catalog_round_trips_without_destroying_history() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCatalog::new(dir.path());
        cache.put_models("dev/mac", "codex", &[model("old-model", "Historical model", "", &[], vec![])]);
        cache.put_harnesses("dev/mac", &fallback_harnesses());
        cache.put_models("dev/mac", "deepseek", &fallback_models("deepseek"));
        assert_eq!(cache.harnesses("dev/mac").unwrap().len(), 1);
        assert_eq!(cache.models("dev/mac", "deepseek").unwrap()[0].id, "deepseek-flash");
        assert_eq!(cache.models("dev/mac", "codex").unwrap()[0].id, "old-model");
    }
}
