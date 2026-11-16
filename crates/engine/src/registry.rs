//! DeepSeek runtime catalog and per-device preferences. Legacy identifiers are read-only history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use zeron_harness::{Harness, HarnessError, mock::MockHarness};
use zeron_proto::{AccessMode, AgentEvent, DoneStatus, HarnessId, ReasoningLevel, SteeringMode};

/// What `ListHarnesses` reports per harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessDescriptor {
    pub id: HarnessId,
    pub name: String,
    pub supports_steering: bool,
    pub steering_mode: SteeringMode,
    pub reasoning_levels: Vec<ReasoningLevel>,
    /// Whether the agent's CLI is present on the listing device (the settings
    /// enable-gate). Defaults true so catalogs from engines predating the
    /// field never read as uninstallable.
    #[serde(default = "default_installed")]
    pub installed: bool,
    /// Explicit CLI installation is available on this listing device.
    #[serde(default)]
    pub can_install: bool,
    /// Whether the listing device offers this harness (Settings → Providers).
    /// `None` — the catalog came from an engine predating the setting — means
    /// "unknown": consumers fall back to detection (see [`descriptor_enabled`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The access modes this harness honors, least to most trusting. Empty
    /// — a catalog from an engine predating modes — offers no choice: those
    /// engines run every chat with full access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_modes: Vec<AccessMode>,
    /// Whether this harness honors plan mode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub supports_plan: bool,
    /// An agent added from the ACP registry: its one-ink SVG icon, for the
    /// pickers (built-in agents have marks of their own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl HarnessDescriptor {
    /// Whether this harness can accept a prompt inside the turn that is
    /// currently running. Turn-boundary steering is still useful to the
    /// automatic queue drain, but it is not the non-interrupting "Steer"
    /// action exposed on an individual queued row.
    pub fn steers_mid_turn(&self) -> bool {
        self.supports_steering && self.steering_mode == SteeringMode::StepBoundary
    }
}

fn default_installed() -> bool {
    true
}

/// Whether detection alone may switch a harness on. The mock always "resolves"
/// but is a test rig, and counting it would let the last real agent be turned
/// off (the composer needs something it can actually run); the dev rig opts it
/// in through `COMET_HARNESS=mock`, which bypasses enablement in the pickers.
fn auto_enabled(id: HarnessId) -> bool {
    id == HarnessId::DeepSeek
}

/// A descriptor's effective enabled flag. `None` — a catalog from an engine
/// predating the setting — falls back to detection, the same rule new devices
/// start from (see [`HarnessRegistry::enabled_set`]).
pub fn descriptor_enabled(descriptor: &HarnessDescriptor) -> bool {
    auto_enabled(descriptor.id) && descriptor
        .enabled
        .unwrap_or_else(|| descriptor.installed && auto_enabled(descriptor.id))
}

fn describe(harness: &dyn Harness) -> HarnessDescriptor {
    HarnessDescriptor {
        id: harness.id(),
        name: harness.display_name().to_string(),
        supports_steering: harness.supports_steering(),
        steering_mode: harness.steering_mode(),
        reasoning_levels: harness.reasoning_levels().to_vec(),
        installed: harness.installed(),
        can_install: false,
        enabled: None,
        access_modes: harness.access_modes().to_vec(),
        supports_plan: harness.supports_plan(),
        icon: None,
    }
}

/// The persisted shape of `harness-prefs.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HarnessPrefsFile {
    /// The user's explicit opt-OUTS. Enablement otherwise follows detection,
    /// so the file only records "no" — an agent installed later turns itself
    /// on without a trip to Settings.
    disabled: Vec<HarnessId>,
    titles: TitleSettings,
    /// Harnesses the user turned native subagents off for: their runs
    /// delegate through side chats only.
    native_subagents_off: Vec<HarnessId>,
    /// Retained for older builds; it never enables removed providers.
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<Vec<HarnessId>>,
}

/// Per-device automatic session title preferences.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TitleSettings {
    /// None follows the session harness, using a supported installed fallback.
    pub harness: Option<HarnessId>,
    /// None selects the cheapest model offered by the selected harness.
    pub model: Option<String>,
}

/// Per-device native subagent preferences (Settings → Providers).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NativeSubagentSettings {
    /// Registered harnesses whose native subagents can be turned off.
    pub switchable: Vec<HarnessId>,
    /// The ones turned off: their runs delegate through side chats only.
    pub off: Vec<HarnessId>,
}

type Factory = Box<dyn Fn() -> Result<Arc<dyn Harness>, HarnessError> + Send + Sync>;
type InstalledProbe = Box<dyn Fn() -> bool + Send + Sync>;

/// The typed twin of [`zeron_proto::CLOUD_HARNESSES`].
pub const CLOUD_HARNESS_IDS: &[HarnessId] = &[HarnessId::DeepSeek];

/// How long a run waits for an expected [`RunEnvironment`]: far longer than the
/// engine's assembly, on any disk.
const RUN_ENVIRONMENT_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Host-side environment for agent spawns (the Cloud device's credential
/// broker). Called right before a request reaches the harness; whatever it
/// puts in [`zeron_proto::RunRequest::env`] is never persisted.
#[async_trait::async_trait]
pub trait RunEnvironment: Send + Sync {
    /// `Err` = the run can't start: no credential for `harness` (the
    /// message says where to sign in). `chat_id` is the chat the run
    /// belongs to (`None` for runs of no chat, such as title generation).
    async fn prepare(
        &self,
        harness: HarnessId,
        chat_id: Option<&str>,
        request: &mut zeron_proto::RunRequest,
    ) -> Result<(), String>;

    /// Which credential a run of `harness` for `chat_id` would use now (an
    /// opaque fingerprint), `None` when unknown. A parked session started
    /// with a different one is replaced before its next turn, so switching
    /// the account applies from the next turn.
    async fn credential(&self, _harness: HarnessId, _chat_id: Option<&str>) -> Option<String> {
        None
    }

    /// A run of `harness` that started with `credential` reported the
    /// subscription's usage (see [`zeron_proto::AgentEvent::PlanUsage`]).
    async fn record_usage(
        &self,
        _harness: HarnessId,
        _credential: &str,
        _windows: Vec<zeron_proto::AgentUsageWindow>,
        _plan_label: Option<String>,
    ) {
    }
}

enum Slot {
    Ready(Arc<dyn Harness>),
    Lazy {
        descriptor: HarnessDescriptor,
        /// Re-run on every `descriptors()` call — a CLI installed mid-session
        /// shows up on the next settings/picker open, no restart needed.
        installed: InstalledProbe,
        factory: Factory,
    },
}

pub struct HarnessRegistry {
    pub(crate) installs: crate::rpc::Installations,
    slots: Mutex<HashMap<HarnessId, Slot>>,
    order: Mutex<Vec<HarnessId>>,
    /// This device's enabled set; `None` inner value = the default set.
    prefs: Mutex<HarnessPrefsFile>,
    /// Where the prefs persist; `None` (tests, bare registries) skips writes.
    prefs_path: Mutex<Option<PathBuf>>,
    /// Fair per-harness execution gates. Runs and title generation hold a
    /// shared lease; an accepted update queues an exclusive lease. Tokio's
    /// write-preferring FIFO policy prevents a stream of new runs from
    /// starving an update that is already waiting.
    gates: Mutex<HashMap<HarnessId, Arc<tokio::sync::RwLock<()>>>>,
    pending_updates: Mutex<std::collections::HashSet<HarnessId>>,
    update_generation: tokio::sync::watch::Sender<u64>,
    /// Spawn-time environment provider (Cloud runner only).
    run_environment: Mutex<Option<Arc<dyn RunEnvironment>>>,
    /// False from [`Self::expect_run_environment`] until it's installed.
    run_environment_ready: tokio::sync::watch::Sender<bool>,
    /// Spawn-time environment of the agent accounts zeron keeps itself
    /// (Muse's per-account roots); after the Cloud broker, never over it.
    account_environment: Mutex<Option<Arc<dyn RunEnvironment>>>,
    /// Credits Zeron in agents' commits ([`crate::commit_credit`]).
    commit_credit: std::sync::OnceLock<Arc<crate::commit_credit::CommitCredit>>,
}

impl Default for HarnessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessRegistry {
    pub async fn discover_models(
        &self,
        id: HarnessId,
    ) -> Result<Vec<zeron_proto::Model>, HarnessError> {
        let lease = Arc::new(self.execution_lease(id).await);
        self.discover_models_with_lease(id, lease).await
    }

    /// A caller such as titling already holds a lease. Reacquiring after an
    /// update queues would deadlock it against its own existing reader.
    pub(crate) async fn discover_models_with_lease(
        &self,
        id: HarnessId,
        lease: Arc<tokio::sync::OwnedRwLockReadGuard<()>>,
    ) -> Result<Vec<zeron_proto::Model>, HarnessError> {
        let harness = self.resolve(id)?;
        tokio::spawn(async move {
            // An RPC cancellation must not drop the gate before the probe's
            // own deadline and child cleanup finish.
            let _lease = lease;
            harness.models().await
        })
        .await
        .map_err(|error| HarnessError::Protocol(format!("model discovery task failed: {error}")))?
    }

    pub async fn discover_commands(
        &self,
        id: HarnessId,
        cwd: &Path,
    ) -> Result<Vec<zeron_proto::SlashCommand>, HarnessError> {
        let cwd = cwd.to_owned();
        let lease = self.execution_lease(id).await;
        let harness = self.resolve(id)?;
        tokio::spawn(async move {
            let _lease = lease;
            harness.commands_for(&cwd).await
        })
        .await
        .map_err(|error| {
            HarnessError::Protocol(format!("command discovery task failed: {error}"))
        })?
    }

    pub async fn discover_skills(
        &self,
        id: HarnessId,
        cwd: &Path,
    ) -> Result<Option<Vec<zeron_proto::invocation::Skill>>, HarnessError> {
        let cwd = cwd.to_owned();
        let lease = self.execution_lease(id).await;
        let harness = self.resolve(id)?;
        tokio::spawn(async move {
            // Retain the read lease through the adapter's deadline and cleanup,
            // even when the requesting RPC is dropped.
            let _lease = lease;
            harness.skills(&cwd).await
        })
        .await
        .map_err(|error| HarnessError::Protocol(format!("skill discovery task failed: {error}")))?
    }

    pub fn new() -> Self {
        let (update_generation, _) = tokio::sync::watch::channel(0);
        Self {
            installs: Default::default(),
            slots: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            prefs: Mutex::new(HarnessPrefsFile::default()),
            prefs_path: Mutex::new(None),
            gates: Mutex::new(HashMap::new()),
            pending_updates: Mutex::new(std::collections::HashSet::new()),
            update_generation,
            run_environment: Mutex::new(None),
            run_environment_ready: tokio::sync::watch::channel(true).0,
            account_environment: Mutex::new(None),
            commit_credit: std::sync::OnceLock::new(),
        }
    }

    /// Install the spawn-time environment provider (the Cloud runner's
    /// credential broker). Laptops never set one.
    pub fn set_run_environment(&self, environment: Arc<dyn RunEnvironment>) {
        *self
            .run_environment
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(environment);
        self.run_environment_ready.send_replace(true);
    }

    /// One is on its way: runs wait for it rather than start without it. On a
    /// Cloud machine, boot recovery dispatches a killed turn while the engine
    /// is still assembling, ahead of the credential broker. Started then, the
    /// agent has no login and none of its earlier conversations, and it fails.
    pub fn expect_run_environment(&self) {
        self.run_environment_ready.send_replace(false);
    }

    /// Install the agent accounts' environment provider (every device).
    pub fn set_account_environment(&self, environment: Arc<dyn RunEnvironment>) {
        *self
            .account_environment
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(environment);
    }

    fn environments(&self) -> Vec<Arc<dyn RunEnvironment>> {
        [&self.run_environment, &self.account_environment]
            .into_iter()
            .filter_map(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).clone())
            .collect()
    }

    pub fn set_commit_credit(&self, credit: Arc<crate::commit_credit::CommitCredit>) {
        let _ = self.commit_credit.set(credit);
    }

    pub fn commit_credit(&self) -> Option<&Arc<crate::commit_credit::CommitCredit>> {
        self.commit_credit.get()
    }

    /// Fill `request.env` for a spawn of `harness` (no-op without a provider),
    /// and credit Zeron in its commits when that's on.
    pub async fn prepare_run(
        &self,
        harness: HarnessId,
        chat_id: Option<&str>,
        request: &mut zeron_proto::RunRequest,
    ) -> Result<(), String> {
        crate::sessions::ensure_run_harness(harness).map_err(|error| error.to_string())?;
        let mut ready = self.run_environment_ready.subscribe();
        if tokio::time::timeout(RUN_ENVIRONMENT_WAIT, ready.wait_for(|ready| *ready))
            .await
            .is_err()
        {
            return Err("This machine's agent logins aren't ready yet. Send again in a moment.".into());
        }
        for environment in self.environments() {
            environment.prepare(harness, chat_id, request).await?;
        }
        if let Some(credit) = self.commit_credit.get() {
            credit.apply(&mut request.env);
        }
        Ok(())
    }

    /// [`RunEnvironment::credential`] (`None` without a provider).
    pub async fn run_credential(
        &self,
        harness: HarnessId,
        chat_id: Option<&str>,
    ) -> Option<String> {
        for environment in self.environments() {
            if let Some(credential) = environment.credential(harness, chat_id).await {
                return Some(credential);
            }
        }
        None
    }

    /// Hand a run's reported subscription usage to every provider (the
    /// account it belongs to records it).
    pub async fn record_plan_usage(
        &self,
        harness: HarnessId,
        credential: &str,
        windows: Vec<zeron_proto::AgentUsageWindow>,
        plan_label: Option<String>,
    ) {
        for environment in self.environments() {
            environment
                .record_usage(harness, credential, windows.clone(), plan_label.clone())
                .await;
        }
    }

    /// Whether `id` exists on this device at all (a Cloud device registers
    /// only [`CLOUD_HARNESS_IDS`]).
    pub fn is_registered(&self, id: HarnessId) -> bool {
        self.slots().contains_key(&id)
    }

    fn gate(&self, id: HarnessId) -> Arc<tokio::sync::RwLock<()>> {
        self.gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
            .clone()
    }

    /// Shared lease held for the full lifetime of a harness subprocess.
    pub async fn execution_lease(&self, id: HarnessId) -> tokio::sync::OwnedRwLockReadGuard<()> {
        let mut updates = self.update_generation.subscribe();
        loop {
            // The marker closes the small begin-update → writer-future polling
            // gap. Watch retains a generation change, so an update finishing
            // between this check and `changed()` cannot lose the wakeup.
            if self.update_pending(id) {
                let _ = updates.changed().await;
                continue;
            }
            let lease = self.gate(id).read_owned().await;
            if !self.update_pending(id) {
                return lease;
            }
            drop(lease);
        }
    }

    /// Exclusive lease held from immediately before update mutation through
    /// post-install verification.
    pub async fn update_lease(&self, id: HarnessId) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.gate(id).write_owned().await
    }

    /// The exclusive lease, if nothing is using this CLI right now.
    pub fn try_update_lease(&self, id: HarnessId) -> Option<tokio::sync::OwnedRwLockWriteGuard<()>> {
        self.gate(id).try_write_owned().ok()
    }

    /// Mark an accepted update before queueing its writer. Existing persistent
    /// runtimes use this signal to retire at their next turn boundary instead
    /// of parking indefinitely while the writer waits.
    pub fn begin_update(&self, id: HarnessId) {
        self.pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id);
    }

    pub fn end_update(&self, id: HarnessId) {
        self.pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        self.update_generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    pub fn update_pending(&self, id: HarnessId) -> bool {
        self.pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id)
    }

    /// Run a synchronous dispatch-boundary action only if no update has been
    /// accepted for this harness. Holding the marker lock through the action
    /// gives direct steering a strict order against `begin_update`: either the
    /// prompt is accepted first and belongs to the existing run, or it waits
    /// behind the update through the ordinary dispatch path.
    pub(crate) fn while_update_clear<T>(
        &self,
        id: HarnessId,
        action: impl FnOnce() -> T,
    ) -> Option<T> {
        let pending = self
            .pending_updates
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if pending.contains(&id) {
            None
        } else {
            Some(action())
        }
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<HarnessId, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn order(&self) -> MutexGuard<'_, Vec<HarnessId>> {
        self.order.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn prefs(&self) -> MutexGuard<'_, HarnessPrefsFile> {
        self.prefs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Load `harness-prefs.json` from the engine data dir and remember the
    /// path for writes. Corrupt/missing files fall back to the default set.
    pub fn load_prefs(&self, data_dir: &Path) {
        let path = data_dir.join("harness-prefs.json");
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HarnessPrefsFile>(&text).ok())
            .unwrap_or_default();
        *self.prefs() = loaded;
        *self
            .prefs_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(path);
    }

    /// What this device offers: every harness whose CLI is FOUND, minus the
    /// user's explicit opt-outs. Enablement follows detection, so installing
    /// an agent is all it takes for it to appear in the composer.
    pub fn enabled_set(&self) -> Vec<HarnessId> {
        // Both guards drop before the installed probes run: `descriptors()`
        // takes `slots` then `order`, so holding `order` across a probe (which
        // takes `slots`) would invert the lock order.
        let registered: Vec<HarnessId> = self.order().iter().copied().collect();
        let disabled = self.prefs().disabled.clone();
        registered
            .into_iter()
            .filter(|id| auto_enabled(*id) && !disabled.contains(id) && self.installed_for(*id))
            .collect()
    }

    /// Whether this device's CLI probe passes for `id` (no spawn, no resolve).
    fn installed_for(&self, id: HarnessId) -> bool {
        match self.slots().get(&id) {
            Some(Slot::Ready(harness)) => harness.installed(),
            Some(Slot::Lazy { installed, .. }) => installed(),
            None => false,
        }
    }

    /// Flip one harness's enablement and persist. Refuses unknown harnesses,
    /// enabling one whose CLI is missing (the settings gate, enforced where
    /// the state lives), and disabling the last enabled harness — under
    /// detection-based enablement everything enabled is runnable, so the
    /// last one standing is always worth protecting (the composer needs
    /// something to run). A harness whose CLI is missing is never enabled
    /// in the first place, so turning it off is a clean no-op.
    pub fn set_enabled(&self, id: HarnessId, on: bool) -> Result<(), String> {
        if !self.slots().contains_key(&id) {
            return Err(format!("unknown harness {id:?}"));
        }
        if on && !auto_enabled(id) {
            return Err(format!("{id:?} cannot be enabled from Settings"));
        }
        if on && !self.installed_for(id) {
            return Err(format!("{id:?} CLI is not installed on this device"));
        }
        let enabled = self.enabled_set();
        match (on, enabled.contains(&id)) {
            (true, false) => {
                let mut prefs = self.prefs();
                prefs.disabled.retain(|h| *h != id);
            }
            (false, true) => {
                if enabled.len() == 1 {
                    return Err("cannot disable the last enabled harness".into());
                }
                let mut prefs = self.prefs();
                prefs.disabled.push(id);
            }
            _ => return Ok(()),
        }
        self.persist_prefs();
        Ok(())
    }

    /// Best-effort atomic write (temp + rename, the ui-settings pattern).
    fn persist_prefs(&self) {
        let Some(path) = self
            .prefs_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        let json = match serde_json::to_string_pretty(&*self.prefs()) {
            Ok(json) => json,
            Err(err) => {
                tracing::warn!(error = %err, "harness-prefs serialize failed");
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(err) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)) {
            tracing::warn!(error = %err, "harness-prefs save failed");
        }
    }

    pub fn title_settings(&self) -> TitleSettings {
        self.prefs().titles.clone()
    }

    pub fn set_title_settings(&self, mut settings: TitleSettings) -> Result<(), String> {
        if let Some(id) = settings.harness {
            let enabled = self.enabled_set().contains(&id)
                || (cfg!(debug_assertions) && id == HarnessId::Mock && self.is_registered(id));
            if !zeron_harness::supports_titles(id) || !enabled {
                return Err("Choose an enabled harness that supports title generation".into());
            }
        } else if settings.model.is_some() {
            return Err("Choose a title harness before choosing a model".into());
        }
        settings.model = settings.model.filter(|model| !model.trim().is_empty());
        self.prefs().titles = settings;
        self.persist_prefs();
        Ok(())
    }

    /// Whether runs of `id` keep the provider's own subagent tools. Off only
    /// by the user's choice, for a driver that can launch without them.
    pub fn native_subagents(&self, id: HarnessId) -> bool {
        !(zeron_harness::switches_native_subagents(id)
            && self.prefs().native_subagents_off.contains(&id))
    }

    pub fn native_subagent_settings(&self) -> NativeSubagentSettings {
        let registered: Vec<HarnessId> = self.order().iter().copied().collect();
        NativeSubagentSettings {
            switchable: registered
                .into_iter()
                .filter(|id| zeron_harness::switches_native_subagents(*id))
                .collect(),
            off: self.prefs().native_subagents_off.clone(),
        }
    }

    /// Turn `id`'s native subagents on or off and persist. Each chat picks
    /// the change up at its next turn once its background work is done: a
    /// parked session still holding some is kept as it started.
    pub fn set_native_subagents(&self, id: HarnessId, on: bool) -> Result<(), String> {
        if !zeron_harness::switches_native_subagents(id) {
            return Err(format!("{id:?} has no native subagents to turn off"));
        }
        {
            let mut prefs = self.prefs();
            match (on, prefs.native_subagents_off.contains(&id)) {
                (true, true) => prefs.native_subagents_off.retain(|h| *h != id),
                (false, false) => prefs.native_subagents_off.push(id),
                _ => return Ok(()),
            }
        }
        self.persist_prefs();
        Ok(())
    }

    pub fn register(&self, harness: Arc<dyn Harness>) {
        let id = harness.id();
        if !id.is_supported() { return; }
        if self.slots().insert(id, Slot::Ready(harness)).is_none() {
            self.order().push(id);
        }
    }

    /// Register a slot resolved on first `resolve` (the factory result is
    /// cached). `installed` is the CLI-presence probe run per `descriptors()`
    /// call; it must never spawn.
    pub fn register_lazy(
        &self,
        descriptor: HarnessDescriptor,
        installed: InstalledProbe,
        factory: Factory,
    ) {
        let id = descriptor.id;
        if !id.is_supported() { return; }
        if self
            .slots()
            .insert(
                id,
                Slot::Lazy {
                    descriptor,
                    installed,
                    factory,
                },
            )
            .is_none()
        {
            self.order().push(id);
        }
    }

    pub fn descriptors_for(&self, _include_registry: bool) -> Vec<HarnessDescriptor> {
        self.descriptors()
    }

    pub fn resolve(&self, id: HarnessId) -> Result<Arc<dyn Harness>, HarnessError> {
        crate::sessions::ensure_run_harness(id)
            .map_err(|error| HarnessError::Protocol(error.to_string()))?;
        let mut slots = self.slots();
        match slots.get(&id) {
            Some(Slot::Ready(harness)) => Ok(harness.clone()),
            Some(Slot::Lazy { factory, .. }) => {
                let harness = factory()?;
                slots.insert(id, Slot::Ready(harness.clone()));
                Ok(harness)
            }
            None => Err(HarnessError::NotInstalled(format!("{id:?}"))),
        }
    }

    /// Catalog for `ListHarnesses` — never forces a lazy resolve.
    pub fn descriptors(&self) -> Vec<HarnessDescriptor> {
        let enabled = self.enabled_set();
        let slots = self.slots();
        self.order()
            .iter()
            .filter_map(|id| {
                let mut descriptor = match slots.get(id) {
                    Some(Slot::Ready(harness)) => describe(harness.as_ref()),
                    Some(Slot::Lazy {
                        descriptor,
                        installed,
                        ..
                    }) => HarnessDescriptor {
                        installed: installed(),
                        ..descriptor.clone()
                    },
                    None => return None,
                };
                descriptor.enabled = Some(enabled.contains(id));
                descriptor.can_install = zeron_harness::install::can_install(*id);
                Some(descriptor)
            })
            .collect()
    }
}

/// The production registry for this engine's platform (see
/// [`registry_for_platform`]).
pub fn default_registry() -> HarnessRegistry {
    registry_for_platform(&crate::runner::device_platform())
}

pub fn registry_for_platform(_platform: &str) -> HarnessRegistry {
    full_registry()
}

fn full_registry() -> HarnessRegistry {
    zeron_harness::shell_env::prewarm();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(zeron_harness::DeepSeekHarness::new()));
    if cfg!(debug_assertions) && std::env::var("COMET_HARNESS").as_deref() == Ok("mock") {
        registry.register(Arc::new(MockHarness {
            script: vec![AgentEvent::Done {
                status: DoneStatus::Completed,
                result: Some("Mock harness".into()),
                error: None,
                session_id: None,
            }],
        }));
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mid_turn_steering_requires_support_and_a_step_boundary() {
        let mut descriptor = HarnessDescriptor {
            icon: None,
            id: HarnessId::Mock,
            name: "Mock".into(),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            reasoning_levels: Vec::new(),
            installed: true,
            can_install: false,
            enabled: Some(true),
            access_modes: Vec::new(),
            supports_plan: false,
        };
        assert!(descriptor.steers_mid_turn());

        descriptor.steering_mode = SteeringMode::TurnBoundary;
        assert!(!descriptor.steers_mid_turn());

        descriptor.steering_mode = SteeringMode::StepBoundary;
        descriptor.supports_steering = false;
        assert!(!descriptor.steers_mid_turn());
    }

    #[test]
    fn lazy_slot_lists_without_resolving() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let registry = HarnessRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        registry.register_lazy(
            HarnessDescriptor {
                icon: None,
                id: HarnessId::Mock,
                name: "Lazy Mock".into(),
                supports_steering: true,
                steering_mode: SteeringMode::StepBoundary,
                reasoning_levels: vec![],
                installed: true,
                can_install: false,
                enabled: None,
                access_modes: Vec::new(),
                supports_plan: false,
            },
            Box::new(|| false),
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Err(HarnessError::NotInstalled("nope".into()))
            }),
        );
        let listed = registry.descriptors();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Lazy Mock");
        // The listing runs the probe, not the stored placeholder.
        assert!(!listed[0].installed);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "listing must not force a resolve"
        );
        assert!(registry.resolve(HarnessId::Mock).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// The composer offers what the catalog lists before any harness has
    /// resolved, so each lazy descriptor must offer the modes its harness
    /// honors once it does.
    #[test]
    fn lazy_descriptors_offer_the_modes_their_harness_honors() {
        let registry = full_registry();
        for described in registry.descriptors() {
            let harness = registry.resolve(described.id).unwrap();
            assert_eq!(
                (described.access_modes.as_slice(), described.supports_plan),
                (harness.access_modes(), harness.supports_plan()),
                "{:?}",
                described.id
            );
        }
    }

    #[test]
    fn production_catalog_is_deepseek_only_and_rejects_legacy() {
        let registry = full_registry();
        assert!(registry.is_registered(HarnessId::DeepSeek));
        for slug in ["codex", "claude-code", "cursor", "pi", "opencode", "grok", "muse", "acp.gemini"] {
            let id = HarnessId::from_slug(slug).unwrap();
            assert!(!registry.is_registered(id));
            assert!(registry.resolve(id).err().unwrap().to_string().contains("DeepSeek"));
            assert!(registry.set_enabled(id, true).is_err());
        }
        assert!(registry.descriptors().iter().all(|descriptor|
            descriptor.id == HarnessId::DeepSeek
                || (cfg!(debug_assertions) && descriptor.id == HarnessId::Mock)));
    }

    #[test]
    fn legacy_preferences_are_preserved_without_reenabling_legacy_agents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness-prefs.json");
        let original = r#"{"enabled":["codex"],"disabled":["claude-code"],"titles":{"harness":"codex","model":"gpt-5"},"nativeSubagentsOff":["claude-code"]}"#;
        std::fs::write(&path, original).unwrap();
        let registry = full_registry();
        registry.load_prefs(dir.path());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(registry.title_settings().harness, Some(HarnessId::Codex));
        assert!(registry.resolve(HarnessId::Codex).is_err());
        assert!(registry.set_title_settings(TitleSettings {
            harness: Some(HarnessId::Codex), model: None,
        }).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(registry.native_subagent_settings().switchable.is_empty());
    }

    #[test]
    fn mock_is_never_auto_enabled() {
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(MockHarness { script: vec![] }));
        assert!(registry.enabled_set().is_empty());
        assert!(registry.set_enabled(HarnessId::Mock, true).is_err());
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    struct DiscoveryHarness {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl Harness for DiscoveryHarness {
        fn id(&self) -> HarnessId {
            HarnessId::DeepSeek
        }
        fn display_name(&self) -> &str {
            "Discovery fixture"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<zeron_proto::Model>, HarnessError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(vec![])
        }
        async fn commands(&self) -> Result<Vec<zeron_proto::SlashCommand>, HarnessError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(vec![])
        }
        async fn run(
            &self,
            _: zeron_proto::RunRequest,
            _: zeron_harness::RunControls,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<AgentEvent, HarnessError>>,
            HarnessError,
        > {
            unreachable!("discovery must not start a conversation")
        }
    }

    #[tokio::test]
    async fn discovery_waits_for_updates_and_keeps_lease_after_caller_cancellation() {
        use std::time::Duration;
        for commands in [false, true] {
            let registry = Arc::new(HarnessRegistry::new());
            let harness = Arc::new(DiscoveryHarness {
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            registry.register(harness.clone());
            registry.begin_update(HarnessId::DeepSeek);
            let installing = registry.update_lease(HarnessId::DeepSeek).await;
            let caller = tokio::spawn({
                let registry = registry.clone();
                async move {
                    if commands {
                        registry
                            .discover_commands(HarnessId::DeepSeek, Path::new("/tmp"))
                            .await
                            .map(|_| ())
                    } else {
                        registry.discover_models(HarnessId::DeepSeek).await.map(|_| ())
                    }
                }
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(30), harness.started.notified())
                    .await
                    .is_err()
            );
            drop(installing);
            registry.end_update(HarnessId::DeepSeek);
            tokio::time::timeout(Duration::from_secs(1), harness.started.notified())
                .await
                .unwrap();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            registry.begin_update(HarnessId::DeepSeek);
            let mut writer = tokio::spawn({
                let registry = registry.clone();
                async move { registry.update_lease(HarnessId::DeepSeek).await }
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut writer)
                    .await
                    .is_err()
            );
            harness.release.notify_one();
            drop(
                tokio::time::timeout(Duration::from_secs(1), writer)
                    .await
                    .unwrap()
                    .unwrap(),
            );
            registry.end_update(HarnessId::DeepSeek);
        }
    }

    #[tokio::test]
    async fn queued_update_writer_precedes_later_dispatches() {
        let registry = Arc::new(HarnessRegistry::new());
        let running = registry.execution_lease(HarnessId::DeepSeek).await;
        registry.begin_update(HarnessId::DeepSeek);
        assert!(registry.update_pending(HarnessId::DeepSeek));

        let writer_registry = registry.clone();
        let (writer_acquired_tx, writer_acquired_rx) = tokio::sync::oneshot::channel();
        let (release_writer_tx, release_writer_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _writer = writer_registry.update_lease(HarnessId::DeepSeek).await;
            let _ = writer_acquired_tx.send(());
            let _ = release_writer_rx.await;
        });
        tokio::task::yield_now().await;

        let reader_registry = registry.clone();
        let (reader_acquired_tx, reader_acquired_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _reader = reader_registry.execution_lease(HarnessId::DeepSeek).await;
            let _ = reader_acquired_tx.send(());
        });

        drop(running);
        writer_acquired_rx.await.unwrap();
        let mut reader_acquired_rx = Box::pin(reader_acquired_rx);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                &mut reader_acquired_rx
            )
            .await
            .is_err(),
            "later reader jumped ahead of the queued update writer"
        );
        let _ = release_writer_tx.send(());
        registry.end_update(HarnessId::DeepSeek);
        reader_acquired_rx.await.unwrap();
        assert!(!registry.update_pending(HarnessId::DeepSeek));
    }

    #[test]
    fn update_marker_rejects_new_boundary_actions() {
        let registry = HarnessRegistry::new();
        assert_eq!(
            registry.while_update_clear(HarnessId::DeepSeek, || 42),
            Some(42)
        );
        registry.begin_update(HarnessId::DeepSeek);
        let mut called = false;
        assert_eq!(
            registry.while_update_clear(HarnessId::DeepSeek, || called = true),
            None
        );
        assert!(!called);
        registry.end_update(HarnessId::DeepSeek);
    }

    #[test]
    fn cloud_platform_registers_only_deepseek() {
        let registry = registry_for_platform(zeron_proto::CLOUD_PLATFORM);
        assert!(registry.is_registered(HarnessId::DeepSeek));
        assert!(!registry.is_registered(HarnessId::Codex));
        assert!(!registry.is_registered(HarnessId::ClaudeCode));
    }

    #[test]
    fn cloud_harness_ids_mirror_the_proto_contract() {
        // `CLOUD_HARNESSES` uses the wire names, so serialize the typed ids.
        let names: Vec<String> = CLOUD_HARNESS_IDS
            .iter()
            .map(|id| {
                serde_json::to_value(id)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut expected: Vec<&str> = zeron_proto::CLOUD_HARNESSES.to_vec();
        let mut names = names;
        names.sort_unstable();
        expected.sort_unstable();
        assert_eq!(names, expected);
    }

    #[tokio::test]
    async fn run_environment_fills_env_only_when_installed() {
        struct Fill;
        #[async_trait::async_trait]
        impl RunEnvironment for Fill {
            async fn prepare(
                &self,
                harness: HarnessId,
                _chat_id: Option<&str>,
                request: &mut zeron_proto::RunRequest,
            ) -> Result<(), String> {
                request.env.insert("HARNESS".into(), format!("{harness:?}"));
                Ok(())
            }
        }
        let mut request: zeron_proto::RunRequest = serde_json::from_str(
            r#"{"prompt":"p","model":null,"reasoning":null,"cwd":".","sandbox":"workspace-write","resume":null}"#,
        )
        .unwrap();
        let registry = HarnessRegistry::new();
        registry
            .prepare_run(HarnessId::DeepSeek, None, &mut request)
            .await
            .unwrap();
        assert!(request.env.is_empty());
        registry.set_run_environment(Arc::new(Fill));
        registry
            .prepare_run(HarnessId::DeepSeek, None, &mut request)
            .await
            .unwrap();
        assert_eq!(
            request.env.get("HARNESS").map(String::as_str),
            Some("DeepSeek")
        );
    }

    /// Every agent spawn carries the commit-credit hooks while that's on.
    #[tokio::test]
    async fn runs_carry_the_commit_credit_hooks() {
        let data = tempfile::tempdir().unwrap();
        let mut request: zeron_proto::RunRequest = serde_json::from_str(
            r#"{"prompt":"p","model":null,"reasoning":null,"cwd":".","sandbox":"workspace-write","resume":null}"#,
        )
        .unwrap();
        let registry = HarnessRegistry::new();
        let credit = Arc::new(crate::commit_credit::CommitCredit::open(data.path(), false));
        credit.set(true).unwrap();
        registry.set_commit_credit(credit.clone());
        registry
            .prepare_run(HarnessId::DeepSeek, None, &mut request)
            .await
            .unwrap();
        let hooks = data.path().join("git-hooks");
        assert_eq!(
            request.env.get("GIT_CONFIG_KEY_0").map(String::as_str),
            Some("core.hooksPath")
        );
        assert_eq!(
            request.env.get("GIT_CONFIG_VALUE_0").map(String::as_str),
            Some(hooks.to_string_lossy().as_ref())
        );
        assert!(hooks.join("commit-msg").exists());

        // Off on this device: nothing added.
        credit.set(false).unwrap();
        request.env.clear();
        registry
            .prepare_run(HarnessId::DeepSeek, None, &mut request)
            .await
            .unwrap();
        assert!(request.env.is_empty());
    }
}
