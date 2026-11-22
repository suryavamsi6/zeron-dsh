//! `captains.json` is engine-owned state recording which chats are Captains
//! and who is in each crew. The engine is the only writer; the dsh-captains
//! plugin never reads it (it gets its role stamped per run). The Drive
//! directory is the engine's too: `ensure_drive` creates it, idempotently,
//! before a Captain or crew chat's first dispatch.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeron_rpc::{RpcError, methods};

const FILE_NAME: &str = "captains.json";

/// One chat promoted to Captain: its display name, when it was promoted,
/// whether it has since been demoted (its record and crew list are kept as
/// `archived`), and the chat ids of its crew.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Captain {
    pub chat_id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub archived: bool,
    pub crew: Vec<String>,
}

/// The whole `captains.json` file: every Captain on record, live and
/// archived.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    captains: Vec<Captain>,
}

/// The role one chat plays in the run about to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunRole {
    /// Not a live Captain, and not in a live Captain's crew.
    None,
    /// The chat is a live Captain.
    Captain { name: String },
    /// The chat crews for a live Captain.
    Crew {
        captain_id: String,
        captain_name: String,
    },
}

impl Registry {
    /// Read `data_dir/captains.json`. A missing file is an empty registry; a
    /// malformed one is `Err`.
    pub fn load(data_dir: &Path) -> Result<Registry, String> {
        let path = data_dir.join(FILE_NAME);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Registry::default());
            }
            Err(error) => {
                return Err(format!("cannot read {}: {error}", path.display()));
            }
        };
        serde_json::from_str(&raw)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))
    }

    /// Write `data_dir/captains.json` atomically: the directory is created,
    /// the JSON lands in a temp file in the same directory, then renames over
    /// `captains.json`.
    fn save(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join(FILE_NAME);
        std::fs::create_dir_all(data_dir)
            .map_err(|error| format!("cannot create {}: {error}", data_dir.display()))?;
        let mut temp = tempfile::NamedTempFile::new_in(data_dir)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        serde_json::to_writer_pretty(&mut temp, self)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        temp.write_all(b"\n")
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        temp.as_file()
            .sync_all()
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        temp.persist(&path)
            .map_err(|error| format!("cannot write {}: {}", path.display(), error.error))?;
        Ok(())
    }

    /// Every Captain on record, live and archived.
    pub fn captains(&self) -> &[Captain] {
        &self.captains
    }

    /// Whether `chat_id` is a live Captain (an archived one is not).
    pub fn is_captain(&self, chat_id: &str) -> bool {
        self.captains
            .iter()
            .any(|captain| captain.chat_id == chat_id && !captain.archived)
    }

    /// The Captain whose crew lists `chat_id`, if any.
    pub fn captain_of_crew(&self, chat_id: &str) -> Option<&Captain> {
        self.captains.iter().find(|captain| {
            !captain.archived && captain.crew.iter().any(|id| id == chat_id)
        })
    }

    /// Promote `chat_id` to Captain under `name`. A chat that is already a
    /// Captain, or crews for one, is refused. Promoting an archived Captain
    /// reactivates its record and keeps only the crew that can still serve it.
    pub fn promote(&mut self, chat_id: &str, name: &str) -> Result<(), String> {
        if self.is_captain(chat_id) {
            return Err(format!("{chat_id} is already a captain"));
        }
        if let Some(owner) = self.captain_of_crew(chat_id) {
            return Err(format!(
                "a crew member can't be a Captain ({chat_id} crews for {})",
                owner.chat_id
            ));
        }
        // Reactivating an archived record makes its kept crew live again, so it
        // is reconciled first: a member that has since become a live Captain,
        // or joined another live crew, can no longer crew for this one and is
        // dropped. Refusing the reactivation instead would be unreachable
        // through the public API (remove_crew only touches live Captains), so
        // the caller could never clear the stale member first.
        let unavailable: HashSet<String> = self
            .captains
            .iter()
            .filter(|captain| !captain.archived)
            .flat_map(|captain| {
                std::iter::once(captain.chat_id.clone()).chain(captain.crew.iter().cloned())
            })
            .collect();
        match self
            .captains
            .iter_mut()
            .find(|captain| captain.chat_id == chat_id)
        {
            // An archived record is reactivated in place, with whatever of its
            // crew is still free.
            Some(captain) => {
                captain.crew.retain(|member| !unavailable.contains(member));
                captain.name = name.to_string();
                captain.created_at = Utc::now();
                captain.archived = false;
            }
            None => self.captains.push(Captain {
                chat_id: chat_id.to_string(),
                name: name.to_string(),
                created_at: Utc::now(),
                archived: false,
                crew: Vec::new(),
            }),
        }
        Ok(())
    }

    /// Demote `chat_id`'s Captain: its record is kept, marked `archived`,
    /// crew list intact.
    pub fn demote(&mut self, chat_id: &str) -> Result<(), String> {
        let Some(captain) = self
            .captains
            .iter_mut()
            .find(|captain| captain.chat_id == chat_id && !captain.archived)
        else {
            return Err(format!("no such captain: {chat_id}"));
        };
        captain.archived = true;
        Ok(())
    }

    /// Add `chat_id` to `captain_id`'s crew. A Captain can't be crew, and a
    /// chat is in at most one crew.
    pub fn add_crew(&mut self, captain_id: &str, chat_id: &str) -> Result<(), String> {
        if self.is_captain(chat_id) {
            return Err(format!("a Captain can't be crew: {chat_id}"));
        }
        if let Some(owner) = self.captain_of_crew(chat_id) {
            return Err(format!(
                "a chat is in at most one crew ({chat_id} crews for {})",
                owner.chat_id
            ));
        }
        let Some(captain) = self
            .captains
            .iter_mut()
            .find(|captain| captain.chat_id == captain_id && !captain.archived)
        else {
            return Err(format!("no such captain: {captain_id}"));
        };
        captain.crew.push(chat_id.to_string());
        Ok(())
    }

    /// Remove `chat_id` from `captain_id`'s crew.
    pub fn remove_crew(&mut self, captain_id: &str, chat_id: &str) -> Result<(), String> {
        let Some(captain) = self
            .captains
            .iter_mut()
            .find(|captain| captain.chat_id == captain_id && !captain.archived)
        else {
            return Err(format!("no such captain: {captain_id}"));
        };
        let Some(position) = captain.crew.iter().position(|id| id == chat_id) else {
            return Err(format!("{chat_id} is not in that crew"));
        };
        captain.crew.remove(position);
        Ok(())
    }

    /// The role `chat_id` plays in a run: a live Captain, crew of one, or
    /// nothing. An archived Captain and every member of its crew are
    /// `RunRole::None`.
    pub fn run_role(&self, chat_id: &str) -> RunRole {
        if let Some(captain) = self
            .captains
            .iter()
            .find(|captain| captain.chat_id == chat_id && !captain.archived)
        {
            return RunRole::Captain {
                name: captain.name.clone(),
            };
        }
        if let Some(captain) = self.captain_of_crew(chat_id) {
            return RunRole::Crew {
                captain_id: captain.chat_id.clone(),
                captain_name: captain.name.clone(),
            };
        }
        RunRole::None
    }
}

/// `<deepseek_home>/captains` — the root every Captain Drive lives under,
/// and the `driveRoot` the run stamp reports.
pub fn captains_root(deepseek_home: &Path) -> PathBuf {
    deepseek_home.join("captains")
}

/// `<deepseek_home>/captains/<captain_id>` — where a Captain's Drive
/// directory belongs. Pure: computes the path; `ensure_drive` creates it.
/// The id is used as given: validating a request id against the live
/// registry belongs at the RPC boundary (SUR-82) — this module's ids are
/// engine-issued chat ids.
pub fn drive_dir(deepseek_home: &Path, captain_id: &str) -> PathBuf {
    captains_root(deepseek_home).join(captain_id)
}

/// The `ZERON_DEEPSEEK_CAPTAINS` payload a run is stamped with: the role
/// `chat_id` plays in `registry`, or `None` for an ordinary chat. The plugin
/// validates exactly these camelCase keys. `driveRoot` is the parent
/// `<deepseek_home>/captains`; a Captain's own drive lives at [`drive_dir`].
pub fn captains_env(registry: &Registry, deepseek_home: &Path, chat_id: &str) -> Option<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Stamp<'a> {
        role: &'a str,
        chat_id: &'a str,
        captain_id: &'a str,
        captain_name: &'a str,
        drive_root: String,
    }

    let drive_root = captains_root(deepseek_home).to_string_lossy().into_owned();
    let (role, captain_id, captain_name) = match registry.run_role(chat_id) {
        RunRole::None => return None,
        RunRole::Captain { name } => ("captain", chat_id.to_string(), name),
        RunRole::Crew {
            captain_id,
            captain_name,
        } => ("crew", captain_id, captain_name),
    };
    Some(
        serde_json::to_string(&Stamp {
            role,
            chat_id,
            captain_id: &captain_id,
            captain_name: &captain_name,
            drive_root,
        })
        .expect("a captains stamp serialises"),
    )
}

/// The subdirectories every Captain Drive starts with.
const DRIVE_DIRS: [&str; 5] = ["skills", "reports", "renders", "handoffs", "state"];

/// Create `chat_id`'s Captain Drive when its run role has one: a Captain's
/// own `drive_dir(deepseek_home, chat_id)`, or its Captain's drive for a crew
/// chat (the crew chat gets no drive of its own). An ordinary chat is
/// `Ok(None)` — nothing is created, not even the `captains` root.
///
/// The directories are made recursively and the starter files are
/// create-if-absent only, so an edit survives a retry and two simultaneous
/// first calls both succeed with one file per name.
pub fn ensure_drive(
    registry: &Registry,
    deepseek_home: &Path,
    chat_id: &str,
) -> Result<Option<PathBuf>, String> {
    let (captain_id, captain_name) = match registry.run_role(chat_id) {
        RunRole::None => return Ok(None),
        RunRole::Captain { name } => (chat_id.to_string(), name),
        RunRole::Crew {
            captain_id,
            captain_name,
        } => (captain_id, captain_name),
    };
    let drive = captains_root(deepseek_home).join(&captain_id);
    for dir in DRIVE_DIRS {
        let sub = drive.join(dir);
        std::fs::create_dir_all(&sub)
            .map_err(|error| format!("cannot create {}: {error}", sub.display()))?;
    }
    write_starter(
        &drive.join("AGENTS.md"),
        &format!("# {captain_name}: Captain rules\n\nCrew chats never read this file.\n"),
    )?;
    write_starter(
        &drive.join("CREW.md"),
        "# Crew rules\n\nEvery crew chat reads this file on every turn.\n",
    )?;
    write_starter(&drive.join("NOTES.md"), "# Plan and notes\n")?;
    write_starter(
        &drive.join("DIGEST.md"),
        "## Needs you\n\n## New\n\n## In progress\n\n",
    )?;
    Ok(Some(drive))
}

/// Write `contents` to `path` only when `path` does not exist: the first
/// writer wins, and a retry or a concurrent caller gets `AlreadyExists` and
/// leaves the file alone.
fn write_starter(path: &Path, contents: &str) -> Result<(), String> {
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(format!("cannot create {}: {error}", path.display())),
    };
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Serialises every `CaptainsStore` mutation process-wide: two independent
/// stores over the same `captains.json` still take turns under it.
static MUTATION: Mutex<()> = Mutex::new(());

/// The engine's single read/modify/write path over `captains.json`. Every
/// mutation takes a process-wide lock, loads the file fresh under it, applies
/// the same rule checks as `Registry`, saves atomically, and only then
/// publishes the new snapshot — so two racing assignments can never both
/// win. A rule error or a save error leaves the file and the snapshot as
/// they were.
pub struct CaptainsStore {
    data_dir: PathBuf,
    /// The last committed registry; replaced only after a save succeeds.
    snapshot: Mutex<Registry>,
    /// Writes that saved and published — the tests pin "one update per
    /// committed change", so a retry that changes nothing must not count.
    #[cfg(test)]
    commits: std::sync::atomic::AtomicUsize,
}

impl CaptainsStore {
    /// Open the store over `data_dir`, loading `captains.json` once. A
    /// malformed file is `Err`; nothing is published.
    pub fn open(data_dir: PathBuf) -> Result<CaptainsStore, String> {
        let snapshot = Registry::load(&data_dir)?;
        Ok(CaptainsStore {
            data_dir,
            snapshot: Mutex::new(snapshot),
            #[cfg(test)]
            commits: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// A clone of this store's last committed registry (each store keeps its
    /// own; only the write lock is process-wide).
    pub fn snapshot(&self) -> Registry {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Committed writes, for the tests that pin "one update per committed change".
    #[cfg(test)]
    pub fn commits(&self) -> usize {
        self.commits.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// One read/modify/write round: take the process-wide lock, re-read the
    /// file fresh under it, and hand that fresh registry to `change` — the
    /// authoritative checks run there, under the lock that serialises the
    /// write, so the decision is taken on the state actually committed. A
    /// `true` result saves atomically and only then publishes; `false` (an
    /// idempotent retry) commits nothing and is never counted as a write;
    /// an `Err` refusal likewise leaves the file and the snapshot as they
    /// were.
    fn mutate(
        &self,
        change: impl FnOnce(&mut Registry) -> Result<bool, String>,
    ) -> Result<(), String> {
        // Falsifiability anchor: the racing case substitutes this read for the
        // load under the lock (`let mut fresh = _observed;`) and must then see
        // two `Ok` callers. Test-only, so the release path takes no pre-lock read.
        #[cfg(test)]
        let _observed = self.snapshot();
        #[cfg(test)]
        test_seam::arrive();
        let _guard = MUTATION.lock().unwrap_or_else(PoisonError::into_inner);
        let mut fresh = Registry::load(&self.data_dir)?;
        let changed = change(&mut fresh)?;
        if !changed {
            return Ok(());
        }
        fresh.save(&self.data_dir)?;
        *self
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = fresh;
        // The save landed and the snapshot published: exactly one commit.
        #[cfg(test)]
        self.commits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Promote `chat_id` to Captain under `name` (see `Registry::promote`).
    pub fn promote(&self, chat_id: &str, name: &str) -> Result<(), String> {
        self.mutate(|registry| registry.promote(chat_id, name).map(|()| true))
    }

    /// Demote `chat_id`'s Captain (see `Registry::demote`).
    pub fn demote(&self, chat_id: &str) -> Result<(), String> {
        self.mutate(|registry| registry.demote(chat_id).map(|()| true))
    }

    /// Add `chat_id` to `captain_id`'s crew (see `Registry::add_crew`).
    pub fn add_crew(&self, captain_id: &str, chat_id: &str) -> Result<(), String> {
        self.mutate(|registry| {
            registry.add_crew(captain_id, chat_id).map(|()| true)
        })
    }

    /// Remove `chat_id` from `captain_id`'s crew (see
    /// `Registry::remove_crew`).
    pub fn remove_crew(&self, captain_id: &str, chat_id: &str) -> Result<(), String> {
        self.mutate(|registry| {
            registry.remove_crew(captain_id, chat_id).map(|()| true)
        })
    }
}

// ── the five Captain RPC methods ────────────────────────────────────────────

/// Whether this RPC serves `method`.
pub fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::CAPTAINS
            | methods::PROMOTE_CAPTAIN
            | methods::DEMOTE_CAPTAIN
            | methods::ADD_CREW
            | methods::REMOVE_CREW
    )
}

/// One chat row's facts the roster rules read before the registry is touched.
#[derive(Debug, Clone, Default)]
pub struct ChatRow {
    pub space_id: Option<String>,
    pub parent_chat_id: Option<String>,
    pub device_id: String,
    /// The chat's live turn, from the sessions engine.
    pub turn_in_flight: bool,
}

/// The host's chat rows, as the roster rules read them.
pub trait Chats {
    fn chat(&self, chat_id: &str) -> Option<ChatRow>;
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromoteParams {
    chat_id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DemoteParams {
    chat_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CrewParams {
    captain_id: String,
    chat_id: String,
}

/// One Captain as the roster projects it: the registry record plus the Drive
/// path the plugin creates on the Captain's first turn.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RosterEntry {
    chat_id: String,
    name: String,
    created_at: DateTime<Utc>,
    archived: bool,
    crew: Vec<String>,
    drive_path: String,
}

/// The five Captain methods, over the store and the host's chat rows.
pub struct CaptainsRpc<'a> {
    pub store: &'a CaptainsStore,
    pub chats: &'a dyn Chats,
    /// This engine's own device id, from `DocHost::device_id()`.
    pub device_id: &'a str,
    /// The DeepSeek home the roster projects each Captain's Drive path from.
    pub deepseek_home: &'a Path,
}

impl CaptainsRpc<'_> {
    /// Answer one of the five methods. Every successful reply is the same
    /// roster projection, mutator or not.
    pub fn call(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        // `captains.json` is device-local engine state: only this device's
        // own window reads or changes it, and it does so before any params
        // are trusted.
        if !zeron_rpc::caller().is_local() {
            return Err(RpcError::Failed(format!(
                "{method}: the Captain roster is managed from this device's own window"
            )));
        }
        match method {
            methods::CAPTAINS => Ok(self.roster()),
            methods::PROMOTE_CAPTAIN => {
                let params: PromoteParams = zeron_rpc::parse_params(params.clone())?;
                self.promote(&params)
            }
            methods::DEMOTE_CAPTAIN => {
                let params: DemoteParams = zeron_rpc::parse_params(params.clone())?;
                self.demote(&params)
            }
            methods::ADD_CREW => {
                let params: CrewParams = zeron_rpc::parse_params(params.clone())?;
                self.add_crew(&params)
            }
            methods::REMOVE_CREW => {
                let params: CrewParams = zeron_rpc::parse_params(params.clone())?;
                self.remove_crew(&params)
            }
            other => Err(RpcError::UnknownMethod(other.into())),
        }
    }

    /// The whole roster, live and archived records alike, each Captain's
    /// Drive path projected from `deepseek_home`. Pure: nothing here creates
    /// the directory — the plugin does, on the Captain's first turn.
    /// Projection is registry-only: it does not consult the chat store, so
    /// archived or deleted chats appear here as recorded until a repair.
    fn roster(&self) -> Value {
        let registry = self.store.snapshot();
        let captains: Vec<RosterEntry> = registry
            .captains()
            .iter()
            .map(|captain| RosterEntry {
                chat_id: captain.chat_id.clone(),
                name: captain.name.clone(),
                created_at: captain.created_at,
                archived: captain.archived,
                crew: captain.crew.clone(),
                drive_path: drive_dir(self.deepseek_home, &captain.chat_id)
                    .to_string_lossy()
                    .into_owned(),
            })
            .collect();
        json!({ "captains": captains })
    }

    /// The row gate every roster change passes first: the chat exists, lives
    /// on this device, and has no turn in flight a roster change would cut
    /// across (the sessions engine's `turn_in_flight` — the engine keeps no
    /// other busy flag).
    /// A fast pre-check only, read outside `MUTATION`: it produces the early
    /// refusals, and the commit closure re-reads the same facts under the
    /// lock via `committed_row` — which is the decision.
    fn row(&self, chat_id: &str) -> Result<ChatRow, RpcError> {
        let Some(row) = self.chats.chat(chat_id) else {
            return Err(RpcError::BadParams(format!("no such chat: {chat_id}")));
        };
        if row.device_id != self.device_id {
            return Err(RpcError::BadParams(format!(
                "{chat_id} belongs to another device"
            )));
        }
        if row.turn_in_flight {
            return Err(RpcError::BadParams(format!("{chat_id} has a turn in flight")));
        }
        Ok(row)
    }

    /// One chat row re-read under `MUTATION`, with the row gate applied. The
    /// closure that commits a roster change calls this: the facts the commit
    /// depends on are then read under the same lock that serialises the write,
    /// so a chat moved between the pre-check and the commit is observed.
    ///
    /// Lock order: `MUTATION` -> (workspace document lock, sessions statuses).
    /// No engine path takes `MUTATION` while holding either, because this RPC is
    /// the only caller of `CaptainsStore`; a future caller must keep that order or
    /// the two locks can deadlock.
    ///
    /// The residual: a `moveChat`/`deleteChat` that lands between this
    /// under-lock re-read and the atomic rename is still unobserved — a window
    /// now bounded by the save+rename inside the same uninterrupted critical
    /// section, not by the whole pre-check window. A deleted row still leaves
    /// its registry record for a later repair, because the chat store does not
    /// cascade.
    fn committed_row(&self, chat_id: &str) -> Result<ChatRow, String> {
        let Some(row) = self.chats.chat(chat_id) else {
            return Err(format!("no such chat: {chat_id}"));
        };
        if row.device_id != self.device_id {
            return Err(format!("{chat_id} belongs to another device"));
        }
        if row.turn_in_flight {
            return Err(format!("{chat_id} has a turn in flight"));
        }
        Ok(row)
    }

    /// Both rows re-read under `MUTATION`, with the project rule applied to the
    /// state being committed.
    fn committed_crew_rows(
        &self,
        captain_id: &str,
        chat_id: &str,
    ) -> Result<(ChatRow, ChatRow), String> {
        let captain_row = self.committed_row(captain_id)?;
        let member_row = self.committed_row(chat_id)?;
        if captain_row.space_id != member_row.space_id {
            return Err(format!(
                "a crew member must be in the Captain's project ({chat_id} is in another project)"
            ));
        }
        Ok((captain_row, member_row))
    }

    /// `PromoteCaptain`: a retried promote is a committed no-op; renaming a
    /// live Captain is a different ask and refused. An archived record's
    /// reactivation, and the rules on what may promote at all, are the
    /// registry's — checked under the store's lock inside `mutate`.
    /// Promoting a UI-archived chat (`setChatArchived`) is intentional: the
    /// Captains registry does not consult the chat store's archived flag;
    /// archival is a view concern.
    fn promote(&self, params: &PromoteParams) -> Result<Value, RpcError> {
        self.row(&params.chat_id)?;
        self.store
            .mutate(|registry| {
                self.committed_row(&params.chat_id)?;
                if let Some(captain) = registry
                    .captains()
                    .iter()
                    .find(|captain| captain.chat_id == params.chat_id && !captain.archived)
                {
                    return if captain.name == params.name {
                        Ok(false)
                    } else {
                        Err(format!("{} is already a captain", params.chat_id))
                    };
                }
                registry
                    .promote(&params.chat_id, &params.name)
                    .map(|()| true)
            })
            .map_err(RpcError::BadParams)?;
        Ok(self.roster())
    }

    /// `DemoteCaptain`: the record is kept, archived, crew intact; demoting
    /// an already-archived Captain is a committed no-op.
    fn demote(&self, params: &DemoteParams) -> Result<Value, RpcError> {
        self.row(&params.chat_id)?;
        self.store
            .mutate(|registry| {
                self.committed_row(&params.chat_id)?;
                let Some(captain) = registry
                    .captains()
                    .iter()
                    .find(|captain| captain.chat_id == params.chat_id)
                else {
                    return Err(format!("no such captain: {}", params.chat_id));
                };
                if captain.archived {
                    return Ok(false);
                }
                registry.demote(&params.chat_id).map(|()| true)
            })
            .map_err(RpcError::BadParams)?;
        Ok(self.roster())
    }

    /// `AddCrew`: the member must be this device's chat, in the Captain's
    /// project, and — when a child — of a live Captain, not an ordinary
    /// chat (a side conversation has no delegation to serve; a Captain's
    /// own delegation is what crews are for).
    fn add_crew(&self, params: &CrewParams) -> Result<Value, RpcError> {
        let captain_row = self.row(&params.captain_id)?;
        let member_row = self.row(&params.chat_id)?;
        if captain_row.space_id != member_row.space_id {
            return Err(RpcError::BadParams(format!(
                "a crew member must be in the Captain's project ({} is in another project)",
                params.chat_id
            )));
        }
        self.store
            .mutate(|registry| {
                let (_, member_row) =
                    self.committed_crew_rows(&params.captain_id, &params.chat_id)?;
                if let Some(parent) = member_row.parent_chat_id.as_deref()
                    && !registry.is_captain(parent)
                {
                    return Err(format!(
                        "a child of an ordinary chat can't crew: {}",
                        params.chat_id
                    ));
                }
                // A retried add commits nothing; crewing for ANOTHER live
                // Captain is the registry's "at most one crew" refusal.
                if let Some(owner) = registry.captain_of_crew(&params.chat_id)
                    && owner.chat_id == params.captain_id
                {
                    return Ok(false);
                }
                registry
                    .add_crew(&params.captain_id, &params.chat_id)
                    .map(|()| true)
            })
            .map_err(RpcError::BadParams)?;
        Ok(self.roster())
    }

    /// `RemoveCrew`: removing a member that isn't there is a committed
    /// no-op; a Captain that isn't live has no crew to change.
    fn remove_crew(&self, params: &CrewParams) -> Result<Value, RpcError> {
        self.row(&params.captain_id)?;
        self.row(&params.chat_id)?;
        self.store
            .mutate(|registry| {
                self.committed_crew_rows(&params.captain_id, &params.chat_id)?;
                let Some(captain) = registry
                    .captains()
                    .iter()
                    .find(|captain| {
                        captain.chat_id == params.captain_id && !captain.archived
                    })
                else {
                    return Err(format!("no such captain: {}", params.captain_id));
                };
                if captain.crew.iter().any(|id| id == &params.chat_id) {
                    registry
                        .remove_crew(&params.captain_id, &params.chat_id)
                        .map(|()| true)
                } else {
                    Ok(false)
                }
            })
            .map_err(RpcError::BadParams)?;
        Ok(self.roster())
    }
}

#[cfg(test)]
mod test_seam {
    //! A latch the racing case installs and releases itself. `arrive()` parks
    //! the caller until the test calls `release()`, so the case decides when
    //! the writers contend for the lock; no writer waits on another writer's
    //! arrival. A no-op when no latch is installed, and compiled out of
    //! release builds.

    use std::sync::{Arc, Condvar, Mutex, PoisonError};

    #[derive(Default)]
    struct State {
        arrived: usize,
        released: bool,
    }

    /// Parks every `arrive()` until the test releases it.
    pub(super) struct Latch {
        state: Mutex<State>,
        released: Condvar,
    }

    impl Latch {
        pub(super) fn new() -> Latch {
            Latch {
                state: Mutex::new(State::default()),
                released: Condvar::new(),
            }
        }

        /// Park until the test releases the latch.
        pub(super) fn arrive(&self) {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut state = state;
            state.arrived += 1;
            self.released.notify_all();
            let _state = self
                .released
                .wait_while(state, |state| !state.released)
                .unwrap_or_else(PoisonError::into_inner);
        }

        /// Wait until `count` callers have parked.
        pub(super) fn wait_arrived(&self, count: usize) {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let _state = self
                .released
                .wait_while(state, |state| state.arrived < count)
                .unwrap_or_else(PoisonError::into_inner);
        }

        /// Let every parked caller continue.
        pub(super) fn release(&self) {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut state = state;
            state.released = true;
            self.released.notify_all();
        }
    }

    static LATCH: Mutex<Option<Arc<Latch>>> = Mutex::new(None);

    pub(super) fn install(latch: Arc<Latch>) {
        *LATCH.lock().unwrap_or_else(PoisonError::into_inner) = Some(latch);
    }

    pub(super) fn clear() {
        *LATCH.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(super) fn arrive() {
        let latch = LATCH.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(latch) = latch {
            latch.arrive();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn load_of_a_missing_file_is_an_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::load(dir.path()).unwrap();
        assert!(registry.captains().is_empty());
    }

    #[test]
    fn save_then_reopen_round_trips_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        registry.add_crew("chat-1", "chat-2").unwrap();
        registry.save(dir.path()).unwrap();

        let raw = std::fs::read_to_string(dir.path().join("captains.json")).unwrap();
        assert!(raw.contains("\"chatId\""), "{raw}");
        assert!(raw.contains("\"createdAt\""), "{raw}");
        assert!(raw.contains("\"crew\""), "{raw}");

        let reopened = Registry::load(dir.path()).unwrap();
        assert_eq!(reopened, registry);
    }

    #[test]
    fn promote_creates_a_captain_and_demote_removes_it() {
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        assert!(registry.is_captain("chat-1"));
        let captain = &registry.captains()[0];
        assert_eq!(captain.chat_id, "chat-1");
        assert_eq!(captain.name, "ada");
        assert!(!captain.archived);
        assert!(captain.crew.is_empty());

        registry.demote("chat-1").unwrap();
        assert!(!registry.is_captain("chat-1"));
        assert_eq!(registry.run_role("chat-1"), RunRole::None);
    }

    #[test]
    fn a_chat_is_in_at_most_one_crew() {
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.promote("cap-b", "grace").unwrap();
        registry.add_crew("cap-a", "chat-x").unwrap();
        let err = registry.add_crew("cap-b", "chat-x").unwrap_err();
        assert!(err.contains("a chat is in at most one crew"), "{err}");
        assert_eq!(registry.captain_of_crew("chat-x").unwrap().chat_id, "cap-a");
    }

    #[test]
    fn a_captain_cannot_be_crew() {
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.promote("cap-b", "grace").unwrap();
        let err = registry.add_crew("cap-a", "cap-b").unwrap_err();
        assert!(err.contains("a Captain can't be crew"), "{err}");
    }

    #[test]
    fn a_crew_member_cannot_be_promoted() {
        let mut registry = Registry::default();
        registry.promote("cap", "ada").unwrap();
        registry.add_crew("cap", "chat-x").unwrap();
        let err = registry.promote("chat-x", "xavier").unwrap_err();
        assert!(err.contains("a crew member can't be a Captain"), "{err}");
        let err = registry.promote("cap", "ada-again").unwrap_err();
        assert!(err.contains("already a captain"), "{err}");
    }

    #[test]
    fn an_archived_captain_and_its_crew_resolve_to_none() {
        let mut registry = Registry::default();
        registry.promote("cap", "ada").unwrap();
        registry.add_crew("cap", "crew-1").unwrap();
        registry.demote("cap").unwrap();
        let captain = registry
            .captains()
            .iter()
            .find(|captain| captain.chat_id == "cap")
            .unwrap();
        assert!(captain.archived, "a demoted Captain stays on record");
        assert_eq!(registry.run_role("cap"), RunRole::None);
        assert_eq!(registry.run_role("crew-1"), RunRole::None);
    }

    /// The rules that hold for every live registry: no live Captain is in a
    /// live crew, and no chat crews for two live Captains.
    fn assert_live_rules_hold(registry: &Registry) {
        let live: Vec<&Captain> = registry
            .captains()
            .iter()
            .filter(|captain| !captain.archived)
            .collect();
        let captains: Vec<&str> = live.iter().map(|captain| captain.chat_id.as_str()).collect();
        let mut crewed: Vec<&str> = Vec::new();
        for captain in &live {
            for member in &captain.crew {
                assert!(
                    !captains.contains(&member.as_str()),
                    "a live Captain is crew: {member} crews for {}",
                    captain.chat_id
                );
                assert!(
                    !crewed.contains(&member.as_str()),
                    "a chat is in two live crews: {member}"
                );
                crewed.push(member);
            }
        }
    }

    #[test]
    fn promoting_an_archived_captain_drops_a_member_that_became_a_captain() {
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.add_crew("cap-a", "chat-x").unwrap();
        registry.demote("cap-a").unwrap();
        // A demoted Captain keeps its crew on record, and a chat whose captain
        // is archived is free: chat-x can become a Captain itself.
        registry.promote("chat-x", "xavier").unwrap();
        registry.promote("cap-a", "ada again").unwrap();

        let reactivated = registry
            .captains()
            .iter()
            .find(|captain| captain.chat_id == "cap-a")
            .unwrap();
        assert!(!reactivated.archived);
        assert!(
            reactivated.crew.is_empty(),
            "a live Captain cannot crew: {:?}",
            reactivated.crew
        );
        assert_live_rules_hold(&registry);
    }

    #[test]
    fn promoting_an_archived_captain_drops_a_member_of_another_crew() {
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.add_crew("cap-a", "chat-x").unwrap();
        registry.demote("cap-a").unwrap();
        // With cap-a archived, chat-x is in nobody's live crew, so another
        // Captain can take it.
        registry.promote("cap-b", "grace").unwrap();
        registry.add_crew("cap-b", "chat-x").unwrap();
        registry.promote("cap-a", "ada again").unwrap();

        let reactivated = registry
            .captains()
            .iter()
            .find(|captain| captain.chat_id == "cap-a")
            .unwrap();
        assert!(
            reactivated.crew.is_empty(),
            "chat-x crews for cap-b: {:?}",
            reactivated.crew
        );
        assert_live_rules_hold(&registry);
    }

    #[test]
    fn promoting_an_archived_captain_keeps_a_crew_that_is_still_free() {
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.add_crew("cap-a", "chat-x").unwrap();
        registry.demote("cap-a").unwrap();
        registry.promote("cap-a", "ada again").unwrap();

        let reactivated = registry
            .captains()
            .iter()
            .find(|captain| captain.chat_id == "cap-a")
            .unwrap();
        assert_eq!(reactivated.crew, vec!["chat-x".to_string()]);
        assert_eq!(reactivated.name, "ada again");
        assert_live_rules_hold(&registry);
    }

    #[test]
    fn remove_crew_drops_a_member_and_refuses_a_stranger() {
        let mut registry = Registry::default();
        registry.promote("cap", "ada").unwrap();
        registry.add_crew("cap", "chat-x").unwrap();
        registry.add_crew("cap", "chat-y").unwrap();

        registry.remove_crew("cap", "chat-x").unwrap();
        assert_eq!(
            registry
                .captains()
                .iter()
                .find(|captain| captain.chat_id == "cap")
                .unwrap()
                .crew,
            vec!["chat-y".to_string()]
        );

        let err = registry.remove_crew("cap", "chat-x").unwrap_err();
        assert!(err.contains("is not in that crew"), "{err}");
        let err = registry.remove_crew("nobody", "chat-y").unwrap_err();
        assert!(err.contains("no such captain"), "{err}");
    }

    #[test]
    fn a_store_over_a_missing_file_starts_empty_and_promotes() {
        let dir = tempfile::tempdir().unwrap();
        let store = CaptainsStore::open(dir.path().to_path_buf()).unwrap();
        assert!(store.snapshot().captains().is_empty());

        store.promote("chat-1", "ada").unwrap();
        let reopened = Registry::load(dir.path()).unwrap();
        assert!(reopened.is_captain("chat-1"));
        assert_eq!(store.snapshot(), reopened);
    }

    #[test]
    fn run_role_distinguishes_captain_crew_and_unrelated_chats() {
        let mut registry = Registry::default();
        registry.promote("cap", "ada").unwrap();
        registry.add_crew("cap", "crew-1").unwrap();
        assert_eq!(
            registry.run_role("cap"),
            RunRole::Captain { name: "ada".into() }
        );
        assert_eq!(
            registry.run_role("crew-1"),
            RunRole::Crew {
                captain_id: "cap".into(),
                captain_name: "ada".into()
            }
        );
        assert_eq!(registry.run_role("stranger"), RunRole::None);
    }

    #[test]
    fn drive_dir_is_a_path_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let path = drive_dir(&home, "cap-1");
        assert_eq!(path, home.join("captains").join("cap-1"));
        assert!(
            std::fs::read_dir(&home).unwrap().next().is_none(),
            "drive_dir creates nothing"
        );
    }

    #[test]
    fn captains_env_names_a_captain() {
        let home = Path::new("/deepseek-home");
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();

        let stamp: serde_json::Value =
            serde_json::from_str(&captains_env(&registry, home, "chat-1").unwrap()).unwrap();
        assert_eq!(stamp["role"], "captain");
        assert_eq!(stamp["chatId"], "chat-1");
        assert_eq!(stamp["captainId"], "chat-1");
        assert_eq!(stamp["captainName"], "ada");
        assert_eq!(
            stamp["driveRoot"],
            home.join("captains").to_string_lossy().as_ref()
        );
    }

    #[test]
    fn captains_env_names_a_crew_member() {
        let home = Path::new("/deepseek-home");
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        registry.add_crew("chat-1", "chat-2").unwrap();

        let stamp: serde_json::Value =
            serde_json::from_str(&captains_env(&registry, home, "chat-2").unwrap()).unwrap();
        assert_eq!(stamp["role"], "crew");
        assert_eq!(stamp["chatId"], "chat-2");
        assert_eq!(stamp["captainId"], "chat-1");
        assert_eq!(stamp["captainName"], "ada");
        assert_eq!(
            stamp["driveRoot"],
            home.join("captains").to_string_lossy().as_ref()
        );
    }

    #[test]
    fn captains_env_is_none_for_an_archived_captain() {
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        registry.add_crew("chat-1", "chat-2").unwrap();
        registry.demote("chat-1").unwrap();

        let home = Path::new("/deepseek-home");
        assert_eq!(captains_env(&registry, home, "chat-1"), None);
        assert_eq!(
            captains_env(&registry, home, "chat-2"),
            None,
            "an archived Captain's crew is released too"
        );
    }

    #[test]
    fn captains_env_is_none_for_an_unrelated_chat() {
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        assert_eq!(
            captains_env(&registry, Path::new("/deepseek-home"), "stranger"),
            None
        );
    }

    #[test]
    fn captains_env_serializes_the_keys_the_plugin_validates() {
        let home = Path::new("/deepseek-home");
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        registry.add_crew("chat-1", "chat-2").unwrap();

        for chat_id in ["chat-1", "chat-2"] {
            let stamp: serde_json::Value =
                serde_json::from_str(&captains_env(&registry, home, chat_id).unwrap()).unwrap();
            let mut keys: Vec<&str> = stamp
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                ["captainId", "captainName", "chatId", "driveRoot", "role"],
                "{chat_id} is stamped with exactly the plugin's keys"
            );
        }
    }

    #[test]
    fn ensure_drive_creates_the_exact_layout() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();

        let drive = ensure_drive(&registry, &home, "cap-a").unwrap().unwrap();
        assert_eq!(drive, home.join("captains").join("cap-a"));

        let mut entries: Vec<String> = std::fs::read_dir(&drive)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            [
                "AGENTS.md",
                "CREW.md",
                "DIGEST.md",
                "NOTES.md",
                "handoffs",
                "renders",
                "reports",
                "skills",
                "state",
            ]
        );
        for file in ["AGENTS.md", "CREW.md", "NOTES.md", "DIGEST.md"] {
            assert!(drive.join(file).is_file(), "{file} is not a file");
        }
        for sub in ["skills", "reports", "renders", "handoffs", "state"] {
            assert!(drive.join(sub).is_dir(), "{sub}/ is not a directory");
        }

        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert_eq!(digest, "## Needs you\n\n## New\n\n## In progress\n\n");
    }

    #[test]
    fn ensure_drive_keeps_an_edited_notes_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        let drive = ensure_drive(&registry, &home, "cap-a").unwrap().unwrap();

        let edited = b"# my own notes\nnot the starter\n";
        std::fs::write(drive.join("NOTES.md"), edited).unwrap();
        let again = ensure_drive(&registry, &home, "cap-a").unwrap().unwrap();

        assert_eq!(again, drive);
        assert_eq!(
            std::fs::read(drive.join("NOTES.md")).unwrap(),
            edited,
            "a retry rewrote an edited starter file"
        );
        for file in ["AGENTS.md", "CREW.md", "DIGEST.md"] {
            assert!(drive.join(file).is_file(), "{file} missing after a retry");
        }
    }

    #[test]
    fn ensure_drive_creates_nothing_for_an_ordinary_chat() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        let registry = Registry::default();

        assert_eq!(ensure_drive(&registry, &home, "chat-x").unwrap(), None);
        assert!(
            !home.join("captains").exists(),
            "an ordinary chat created the captains root"
        );
    }

    #[test]
    fn ensure_drive_of_a_crew_member_lands_in_the_captains_drive() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.add_crew("cap-a", "chat-y").unwrap();

        let captain_drive = ensure_drive(&registry, &home, "cap-a").unwrap().unwrap();
        let crew_drive = ensure_drive(&registry, &home, "chat-y").unwrap().unwrap();
        assert_eq!(crew_drive, captain_drive);
        assert_eq!(crew_drive, home.join("captains").join("cap-a"));

        let agents = std::fs::read_to_string(crew_drive.join("AGENTS.md")).unwrap();
        assert!(agents.starts_with("# ada: Captain rules"), "{agents}");
    }

    #[test]
    fn two_racing_assignments_leave_exactly_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.promote("cap-b", "grace").unwrap();
        registry.save(dir.path()).unwrap();

        // Two independent stores over the same file: neither shares the
        // other's snapshot, so only the process-wide lock serialises them.
        let first = CaptainsStore::open(dir.path().to_path_buf()).unwrap();
        let second = CaptainsStore::open(dir.path().to_path_buf()).unwrap();
        // Each writer parks after its pre-lock read; the test, not the other
        // writer, decides when they are released.
        let latch = Arc::new(test_seam::Latch::new());
        test_seam::install(Arc::clone(&latch));
        let results = std::thread::scope(|scope| {
            let a = scope.spawn(|| first.add_crew("cap-a", "chat-x"));
            latch.wait_arrived(1);
            let b = scope.spawn(|| second.add_crew("cap-b", "chat-x"));
            latch.wait_arrived(2);
            latch.release();
            [a.join().unwrap(), b.join().unwrap()]
        });
        test_seam::clear();

        // The discriminator is the result pair, not the file alone: a lost
        // update leaves two `Ok` and a file with one owner, because the second
        // save overwrote the first.
        let committed = Registry::load(dir.path()).unwrap();
        let owners: Vec<&str> = committed
            .captains()
            .iter()
            .filter(|captain| captain.crew.iter().any(|id| id == "chat-x"))
            .map(|captain| captain.chat_id.as_str())
            .collect();
        let callers = ["cap-a", "cap-b"];
        let winners: Vec<&str> = results
            .iter()
            .zip(callers)
            .filter(|(result, _)| result.is_ok())
            .map(|(_, chat_id)| chat_id)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "exactly one assignment commits: {results:?}; persisted owners: {owners:?}"
        );
        let err = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .unwrap();
        assert!(err.contains("a chat is in at most one crew"), "{err}");
        assert_eq!(
            owners, winners,
            "the persisted owner is the caller whose assignment returned Ok"
        );
    }

    #[test]
    fn an_invalid_move_persists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.promote("cap-b", "grace").unwrap();
        registry.add_crew("cap-a", "chat-x").unwrap();
        registry.save(dir.path()).unwrap();
        let file = dir.path().join("captains.json");
        let committed_bytes = std::fs::read(&file).unwrap();

        let store = CaptainsStore::open(dir.path().to_path_buf()).unwrap();
        let committed_snapshot = store.snapshot();
        let err = store.add_crew("cap-b", "chat-x").unwrap_err();
        assert!(err.contains("a chat is in at most one crew"), "{err}");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            committed_bytes,
            "a refused move leaves the file as it was"
        );
        assert_eq!(store.snapshot(), committed_snapshot);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_keeps_the_last_committed_snapshot() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        registry.save(dir.path()).unwrap();
        // (A runner as root writes anyway: nothing here to refuse.)
        if std::fs::metadata(dir.path()).unwrap().uid() == 0 {
            return;
        }
        let store = CaptainsStore::open(dir.path().to_path_buf()).unwrap();
        let committed = store.snapshot();

        let mut permissions = std::fs::metadata(dir.path()).unwrap().permissions();
        permissions.set_mode(0o500);
        std::fs::set_permissions(dir.path(), permissions).unwrap();
        let refused = store.promote("cap-b", "grace");
        let mut permissions = std::fs::metadata(dir.path()).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(dir.path(), permissions).unwrap();
        refused.unwrap_err();

        assert_eq!(
            Registry::load(dir.path()).unwrap(),
            committed,
            "a failed write leaves the last committed registry"
        );
        assert_eq!(store.snapshot(), committed);
    }

    // ── SUR-82: the five Captain RPC methods ────────────────────────────────

    use std::collections::HashMap;

    use serde_json::{Value, json};
    use zeron_rpc::{Caller, RpcError, methods, with_caller};

    /// This engine's own device, as the RPC sees it.
    const DEVICE: &str = "device-1";

    /// The host's chat rows the rule checks read, as a plain map.
    #[derive(Default)]
    struct FakeChats(HashMap<String, ChatRow>);

    impl FakeChats {
        fn row(mut self, chat_id: &str, space: Option<&str>, parent: Option<&str>) -> Self {
            self.0.insert(
                chat_id.to_string(),
                ChatRow {
                    space_id: space.map(str::to_string),
                    parent_chat_id: parent.map(str::to_string),
                    device_id: DEVICE.to_string(),
                    turn_in_flight: false,
                },
            );
            self
        }

        fn other_device(mut self, chat_id: &str) -> Self {
            if let Some(row) = self.0.get_mut(chat_id) {
                row.device_id = "device-2".to_string();
            }
            self
        }

        fn busy(mut self, chat_id: &str) -> Self {
            if let Some(row) = self.0.get_mut(chat_id) {
                row.turn_in_flight = true;
            }
            self
        }
    }

    impl Chats for FakeChats {
        fn chat(&self, chat_id: &str) -> Option<ChatRow> {
            self.0.get(chat_id).cloned()
        }
    }

    /// A store over a temp dir, the chat rows, and the DeepSeek home the
    /// roster projects Drive paths from.
    struct Rig {
        _dir: tempfile::TempDir,
        home: std::path::PathBuf,
        store: CaptainsStore,
        chats: FakeChats,
    }

    impl Rig {
        fn new(chats: FakeChats) -> Rig {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("deepseek");
            std::fs::create_dir(&home).unwrap();
            let store = CaptainsStore::open(dir.path().to_path_buf()).unwrap();
            Rig {
                _dir: dir,
                home,
                store,
                chats,
            }
        }

        fn rpc(&self) -> CaptainsRpc<'_> {
            CaptainsRpc {
                store: &self.store,
                chats: &self.chats,
                device_id: DEVICE,
                deepseek_home: &self.home,
            }
        }

        fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
            self.rpc().call(method, &params)
        }

        fn roster(&self) -> Value {
            self.call(methods::CAPTAINS, json!({})).unwrap()
        }

        fn file(&self) -> Vec<u8> {
            std::fs::read(self._dir.path().join("captains.json")).unwrap_or_default()
        }
    }

    fn entry<'a>(roster: &'a Value, chat_id: &str) -> Option<&'a Value> {
        roster["captains"]
            .as_array()?
            .iter()
            .find(|captain| captain["chatId"] == chat_id)
    }

    fn crew(roster: &Value, chat_id: &str) -> Vec<String> {
        entry(roster, chat_id)
            .and_then(|captain| captain["crew"].as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn bad_params(result: Result<Value, RpcError>, needle: &str) {
        match result {
            Err(RpcError::BadParams(message)) => {
                assert!(
                    message.contains(needle),
                    "the refusal names the wrong rule ({needle:?} missing): {message}"
                );
            }
            other => panic!("expected BadParams naming {needle:?}, got {other:?}"),
        }
    }

    #[test]
    fn the_roster_starts_empty_and_names_each_drive_path() {
        let rig = Rig::new(FakeChats::default().row("cap-a", Some("space-1"), None));
        assert_eq!(
            rig.roster(),
            json!({ "captains": [] }),
            "a fresh store has an empty roster"
        );

        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();

        let roster = rig.roster();
        let captain = entry(&roster, "cap-a").expect("cap-a is in the roster");
        assert_eq!(captain["name"], "Ada");
        assert_eq!(captain["archived"], false);
        assert_eq!(captain["crew"], json!([]));
        let drive = rig.home.join("captains").join("cap-a");
        assert_eq!(
            captain["drivePath"],
            Value::String(drive.to_string_lossy().into_owned()),
            "the roster sends the projected Drive path"
        );
        assert!(
            !drive.exists(),
            "the roster creates no Drive directory: the plugin does, on the first turn"
        );
    }

    #[test]
    fn a_committed_change_is_one_update_and_a_retry_adds_none() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-x", Some("space-1"), None),
        );

        let promote = json!({"chatId": "cap-a", "name": "Ada"});
        rig.call(methods::PROMOTE_CAPTAIN, promote.clone()).unwrap();
        assert_eq!(rig.store.commits(), 1, "a committed change is one update");
        rig.call(methods::PROMOTE_CAPTAIN, promote).unwrap();
        assert_eq!(
            rig.store.commits(),
            1,
            "a retried promote commits nothing"
        );

        let add = json!({"captainId": "cap-a", "chatId": "chat-x"});
        rig.call(methods::ADD_CREW, add.clone()).unwrap();
        assert_eq!(rig.store.commits(), 2);
        rig.call(methods::ADD_CREW, add).unwrap();
        assert_eq!(rig.store.commits(), 2, "a retried add commits nothing");
        assert_eq!(crew(&rig.roster(), "cap-a"), vec!["chat-x".to_string()]);

        let demote = json!({"chatId": "cap-a"});
        rig.call(methods::DEMOTE_CAPTAIN, demote.clone()).unwrap();
        assert_eq!(rig.store.commits(), 3);
        assert_eq!(
            entry(&rig.roster(), "cap-a").unwrap()["archived"],
            true,
            "demote archives the record"
        );
        rig.call(methods::DEMOTE_CAPTAIN, demote).unwrap();
        assert_eq!(
            rig.store.commits(),
            3,
            "a retried demote commits nothing"
        );

        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        assert_eq!(
            rig.store.commits(),
            4,
            "re-promoting an archived Captain reactivates it"
        );
        assert_eq!(entry(&rig.roster(), "cap-a").unwrap()["archived"], false);
        assert_eq!(
            crew(&rig.roster(), "cap-a"),
            vec!["chat-x".to_string()],
            "a crew that is still free comes back with the reactivation"
        );
    }

    #[test]
    fn add_crew_refuses_a_chat_from_another_project() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-x", Some("space-2"), None)
                .row("chat-y", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        let before = rig.file();

        bad_params(
            rig.call(
                methods::ADD_CREW,
                json!({"captainId": "cap-a", "chatId": "chat-x"}),
            ),
            "project",
        );
        assert!(
            crew(&rig.roster(), "cap-a").is_empty(),
            "a refused add leaves the prior membership alone"
        );
        assert_eq!(rig.file(), before, "a refused add writes nothing");

        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-y"}),
        )
        .unwrap();
        assert_eq!(crew(&rig.roster(), "cap-a"), vec!["chat-y".to_string()]);
    }

    #[test]
    fn add_crew_refuses_another_crews_member_and_a_child_of_an_ordinary_chat() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("cap-b", Some("space-1"), None)
                .row("chat-x", Some("space-1"), None)
                .row("chat-plain", Some("space-1"), None)
                .row("child-z", Some("space-1"), Some("chat-plain"))
                .row("child-w", Some("space-1"), Some("cap-a")),
        );
        for captain in ["cap-a", "cap-b"] {
            rig.call(
                methods::PROMOTE_CAPTAIN,
                json!({"chatId": captain, "name": captain}),
            )
            .unwrap();
        }
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-b", "chatId": "chat-x"}),
        )
        .unwrap();

        bad_params(
            rig.call(
                methods::ADD_CREW,
                json!({"captainId": "cap-a", "chatId": "chat-x"}),
            ),
            "one crew",
        );
        bad_params(
            rig.call(
                methods::ADD_CREW,
                json!({"captainId": "cap-a", "chatId": "child-z"}),
            ),
            "child",
        );
        assert!(
            crew(&rig.roster(), "cap-a").is_empty(),
            "neither refusal adds a member"
        );

        // A child of a Captain is the rule's other side: it may crew for it.
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "child-w"}),
        )
        .unwrap();
        assert_eq!(crew(&rig.roster(), "cap-a"), vec!["child-w".to_string()]);
    }

    /// A chat row that moves to another project after its first read: the
    /// interleaving a concurrent chat move produces between the pre-check and
    /// the commit. The rule must be checked again on the state actually
    /// committed, not only on the row the pre-check saw.
    struct MovingChat {
        member: String,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl Chats for MovingChat {
        fn chat(&self, chat_id: &str) -> Option<ChatRow> {
            let space = if chat_id == self.member {
                let seen = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if seen >= 1 { "space-2" } else { "space-1" }
            } else {
                "space-1"
            };
            Some(ChatRow {
                space_id: Some(space.to_string()),
                parent_chat_id: None,
                device_id: DEVICE.to_string(),
                turn_in_flight: false,
            })
        }
    }

    #[test]
    fn add_crew_rechecks_the_project_under_the_store_lock() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let store = CaptainsStore::open(dir.path().to_path_buf()).unwrap();
        store.promote("cap-a", "Ada").unwrap();
        assert_eq!(store.commits(), 1);
        let chats = MovingChat {
            member: "chat-x".to_string(),
            reads: std::sync::atomic::AtomicUsize::new(0),
        };
        let rpc = CaptainsRpc {
            store: &store,
            chats: &chats,
            device_id: DEVICE,
            deepseek_home: &home,
        };

        bad_params(
            rpc.call(
                methods::ADD_CREW,
                &json!({"captainId": "cap-a", "chatId": "chat-x"}),
            ),
            "project",
        );
        assert_eq!(
            store.commits(),
            1,
            "a move between the pre-check and the commit must not commit the add"
        );
        let registry = store.snapshot();
        let captain = registry.captains().iter().find(|c| c.chat_id == "cap-a").unwrap();
        assert!(captain.crew.is_empty(), "the roster kept the move out: {:?}", captain.crew);
    }

    #[test]
    fn a_busy_chat_is_refused_before_the_registry_is_written() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-y", Some("space-1"), None)
                .busy("chat-y"),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        let before = rig.file();

        bad_params(
            rig.call(
                methods::ADD_CREW,
                json!({"captainId": "cap-a", "chatId": "chat-y"}),
            ),
            "turn in flight",
        );
        assert_eq!(
            rig.store.commits(),
            1,
            "a busy chat is refused before the commit"
        );
        assert_eq!(rig.file(), before, "a busy refusal writes nothing");
        assert!(crew(&rig.roster(), "cap-a").is_empty());
    }

    #[test]
    fn peer_and_agent_callers_are_refused() {
        let rig = Rig::new(FakeChats::default().row("cap-a", Some("space-1"), None));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for caller in [
            Caller::Peer { device: None },
            Caller::Agent {
                chat: Some("cap-a".to_string()),
            },
        ] {
            let rpc = rig.rpc();
            let refused = runtime.block_on(with_caller(caller, async {
                rpc.call(
                    methods::PROMOTE_CAPTAIN,
                    &json!({"chatId": "cap-a", "name": "Ada"}),
                )
            }));
            match refused {
                Err(RpcError::Failed(message)) => assert!(
                    message.contains(methods::PROMOTE_CAPTAIN),
                    "the refusal names the method: {message}"
                ),
                other => panic!("expected a refused caller, got {other:?}"),
            }
        }
        assert_eq!(rig.store.commits(), 0, "a refused caller writes nothing");

        for method in [
            methods::CAPTAINS,
            methods::PROMOTE_CAPTAIN,
            methods::DEMOTE_CAPTAIN,
            methods::ADD_CREW,
            methods::REMOVE_CREW,
        ] {
            assert!(handles(method), "the RPC serves {method}");
        }
        assert!(!handles(methods::SCHEDULES));
        assert!(!handles(methods::WATCH_CHATS));
    }

    #[test]
    fn params_naming_helm_busy_are_refused() {
        // The registry has no busy state to change, so a params object that
        // tries to set one is refused rather than silently ignored.
        let rig = Rig::new(FakeChats::default().row("cap-a", Some("space-1"), None));
        bad_params(
            rig.call(
                methods::PROMOTE_CAPTAIN,
                json!({"chatId": "cap-a", "name": "Ada", "helm_busy": true}),
            ),
            "helm_busy",
        );
        bad_params(
            rig.call(
                methods::ADD_CREW,
                json!({"captainId": "cap-a", "chatId": "cap-a", "busy": true}),
            ),
            "busy",
        );
        assert_eq!(rig.store.commits(), 0, "a refused params object writes nothing");
    }

    #[test]
    fn a_chat_on_another_device_or_missing_is_refused() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-x", Some("space-1"), None)
                .other_device("chat-x"),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();

        bad_params(
            rig.call(
                methods::ADD_CREW,
                json!({"captainId": "cap-a", "chatId": "chat-x"}),
            ),
            "another device",
        );
        bad_params(
            rig.call(
                methods::ADD_CREW,
                json!({"captainId": "cap-a", "chatId": "ghost"}),
            ),
            "no such chat",
        );
        bad_params(
            rig.call(
                methods::PROMOTE_CAPTAIN,
                json!({"chatId": "ghost", "name": "Nobody"}),
            ),
            "no such chat",
        );
        assert!(crew(&rig.roster(), "cap-a").is_empty());
    }
}
