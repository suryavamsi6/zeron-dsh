//! `captains.json` is engine-owned state recording which chats are Captains
//! and who is in each crew. The engine is the only writer; the dsh-captains
//! plugin never reads it (it gets its role stamped per run). The Drive
//! directory is the engine's too: `ensure_drive` creates it, idempotently,
//! before a Captain or crew chat's first dispatch.
//!
//! The file is the roster's authority for which chats exist: a row deleted
//! after a roster change commits leaves its record in place, because the
//! chat store takes no part in the Captains lock and a delete can land
//! between the under-lock row re-read and the save (SUR-189). Such a record
//! is repaired explicitly rather than guessed at: `DeleteCaptain` drops a
//! Captain record without needing its row, and `RemoveCrew` drops a crew
//! entry whose member row is gone. Until then the roster keeps showing it —
//! the honest report of what was committed, not a claim that the chat still
//! exists.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use zeron_rpc::{RpcError, methods};

const FILE_NAME: &str = "captains.json";

/// File ownership metadata recorded for one crew member. `owns` is not
/// advisory: the run stamp carries it to the DeepSeek policy, which denies
/// edits outside the globs. `worktree` is the dedicated checkout the member
/// is expected to work in; both are optional for older rows and ordinary
/// crew members.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrewMeta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
}

/// One chat promoted to Captain: its display name, when it was promoted,
/// whether it has since been demoted (its record and crew list are kept as
/// `archived`), whether the user has hidden it (`userArchived`, SUR-94), and
/// the chat ids of its crew.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Captain {
    pub chat_id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub archived: bool,
    /// The user-archive flag (SUR-94): written only by archive and
    /// unarchive — `archived` stays promote/demote's own — and either flag
    /// set is not live. A file written before the cascade has no key and
    /// loads as `false`.
    #[serde(default)]
    pub user_archived: bool,
    pub crew: Vec<String>,
    /// The crew chats the archive itself moved (SUR-94): exactly the ids
    /// the unarchive wakes, so a crew chat archived by hand is never the
    /// cascade's business. Kept off the file while it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived_crew: Vec<String>,
    /// Ownership metadata by crew chat id. Kept as a side map so a
    /// `captains.json` written before SUR-206 — plain string crew entries —
    /// loads unchanged, and so existing roster projections keep their shape.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub crew_meta: BTreeMap<String, CrewMeta>,
}

impl Captain {
    /// A live Captain is neither demoted (`archived`) nor hidden by the
    /// user (`userArchived`): the two flags are written by different
    /// actions, and a record carrying either is not live for the stamping
    /// predicates.
    fn live(&self) -> bool {
        !self.archived && !self.user_archived
    }
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
        owns: Vec<String>,
        worktree: Option<String>,
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

    /// Whether `chat_id` is a live Captain (an archived or user-archived
    /// one is not).
    pub fn is_captain(&self, chat_id: &str) -> bool {
        self.captains
            .iter()
            .any(|captain| captain.chat_id == chat_id && captain.live())
    }

    /// The live Captain whose crew lists `chat_id`, if any.
    pub fn captain_of_crew(&self, chat_id: &str) -> Option<&Captain> {
        self.captains.iter().find(|captain| {
            captain.live() && captain.crew.iter().any(|id| id == chat_id)
        })
    }

    /// Promote `chat_id` to Captain under `name`. A chat that is already a
    /// Captain, or crews for one, is refused. Promoting an archived Captain
    /// reactivates its record and keeps only the crew that can still serve it.
    pub fn promote(&mut self, chat_id: &str, name: &str) -> Result<(), String> {
        valid_captain_name(name)?;
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
            .filter(|captain| captain.live())
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
                let crew = captain.crew.clone();
                captain.crew_meta.retain(|member, _| crew.contains(member));
                captain.name = name.to_string();
                captain.created_at = Utc::now();
                captain.archived = false;
            }
            None => self.captains.push(Captain {
                chat_id: chat_id.to_string(),
                name: name.to_string(),
                created_at: Utc::now(),
                archived: false,
                user_archived: false,
                crew: Vec::new(),
                archived_crew: Vec::new(),
                crew_meta: BTreeMap::new(),
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
        self.add_crew_with_metadata(captain_id, chat_id, CrewMeta::default())
    }

    /// Add `chat_id` to `captain_id`'s crew with ownership metadata, or
    /// replace the metadata when the member is already in that crew. The
    /// ownership rules are the same as [`Registry::add_crew`].
    pub fn add_crew_with_metadata(
        &mut self,
        captain_id: &str,
        chat_id: &str,
        meta: CrewMeta,
    ) -> Result<(), String> {
        if meta
            .owns
            .iter()
            .any(|glob| glob.is_empty() || glob.contains('\0'))
        {
            return Err("crew owns globs must be non-empty strings".into());
        }
        if meta
            .worktree
            .as_deref()
            .is_some_and(|path| path.is_empty() || !Path::new(path).is_absolute())
        {
            return Err("crew worktree must be an absolute path".into());
        }
        if self.is_captain(chat_id) {
            return Err(format!("a Captain can't be crew: {chat_id}"));
        }
        if let Some(owner) = self.captain_of_crew(chat_id)
            && owner.chat_id != captain_id
        {
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
        if !captain.crew.iter().any(|id| id == chat_id) {
            captain.crew.push(chat_id.to_string());
        }
        if meta == CrewMeta::default() {
            captain.crew_meta.remove(chat_id);
        } else {
            captain.crew_meta.insert(chat_id.to_string(), meta);
        }
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
        captain.crew_meta.remove(chat_id);
        Ok(())
    }

    /// The archive's half of the cascade (SUR-94): `chat_id`'s record is
    /// flagged `userArchived` and `archivedCrew` keeps exactly the crew ids
    /// `live` answers — the chats the archive itself is putting to sleep,
    /// for the unarchive to wake. `live` is the caller's read of the chat
    /// rows, taken under the store lock on the state being committed: a
    /// crew id with no live row is skipped, never marked, and does not
    /// fail the call. A demoted Captain can never be hidden; a record
    /// already `userArchived` is the retried archive — its kept marker
    /// comes back with `false` so the caller's chat writes re-run while
    /// nothing here commits. The pair is (the ids the caller archives,
    /// whether the registry changed); the marker is set, never appended.
    pub fn archive(
        &mut self,
        chat_id: &str,
        live: impl Fn(&str) -> bool,
    ) -> Result<(Vec<String>, bool), String> {
        let Some(captain) = self
            .captains
            .iter_mut()
            .find(|captain| captain.chat_id == chat_id)
        else {
            return Err(format!("{chat_id} is not a Captain on this device"));
        };
        if captain.archived {
            return Err(format!("a demoted Captain can never be hidden: {chat_id}"));
        }
        if captain.user_archived {
            return Ok((captain.archived_crew.clone(), false));
        }
        let moved: Vec<String> = captain
            .crew
            .iter()
            .filter(|chat_id| live(chat_id))
            .cloned()
            .collect();
        captain.user_archived = true;
        captain.archived_crew = moved.clone();
        Ok((moved, true))
    }

    /// The unarchive's read half: the ids a wake would move — `archivedCrew`
    /// while the record is `userArchived`, and again when it is not but the
    /// marker was never spent (a retry finishing a half-done wake). Empty
    /// when there is nothing to spend. A read only: nothing commits, and the
    /// same ids come back until `unarchive` takes the marker.
    pub fn unarchive_pending(&self, chat_id: &str) -> Result<Vec<String>, String> {
        let Some(captain) = self
            .captains
            .iter()
            .find(|captain| captain.chat_id == chat_id)
        else {
            return Err(format!("{chat_id} is not a Captain on this device"));
        };
        Ok(captain.archived_crew.clone())
    }

    /// The unarchive's finish step: `userArchived` clears and the spent
    /// `archivedCrew` marker is handed back — the chats the caller already
    /// woke off `unarchive_pending`'s read. The woken record is reconciled
    /// the way `promote` reconciles a reactivated one: a kept member that
    /// became a live Captain or joined another live crew while this one
    /// slept is dropped, so no chat is left in two live crews. The marker
    /// is still taken whole — the returned ids are still the ids the
    /// caller woke, whatever the reconcile kept. A record that is not
    /// hidden and has no marker is `Ok((Vec::new(), false))`: nothing to
    /// spend, no commit. The pair is (the ids the caller woke, whether
    /// the registry changed).
    pub fn unarchive(&mut self, chat_id: &str) -> Result<(Vec<String>, bool), String> {
        let Some(captain) = self
            .captains
            .iter_mut()
            .find(|captain| captain.chat_id == chat_id)
        else {
            return Err(format!("{chat_id} is not a Captain on this device"));
        };
        if !captain.user_archived && captain.archived_crew.is_empty() {
            return Ok((Vec::new(), false));
        }
        // Waking the record makes its kept crew live again, so the crew is
        // reconciled like `promote` reconciles a reactivated record: a
        // member that became a live Captain, or joined another live crew,
        // while this one slept is dropped. The record itself is excluded —
        // on the marker-retry path it is already live, and counting it
        // would drop the whole crew it is waking with.
        let unavailable: HashSet<String> = self
            .captains
            .iter()
            .filter(|captain| captain.chat_id != chat_id && captain.live())
            .flat_map(|captain| {
                std::iter::once(captain.chat_id.clone()).chain(captain.crew.iter().cloned())
            })
            .collect();
        let captain = self
            .captains
            .iter_mut()
            .find(|captain| captain.chat_id == chat_id)
            .expect("the lookup above found the record");
        captain.user_archived = false;
        captain.crew.retain(|member| !unavailable.contains(member));
        let crew = captain.crew.clone();
        captain.crew_meta.retain(|member, _| crew.contains(member));
        Ok((std::mem::take(&mut captain.archived_crew), true))
    }

    /// Drop `chat_id`'s record entirely. The chats it names keep their
    /// rows and the Drive folder stays — the record is the registry's only
    /// reach.
    pub fn delete(&mut self, chat_id: &str) -> Result<(), String> {
        let Some(position) = self
            .captains
            .iter()
            .position(|captain| captain.chat_id == chat_id)
        else {
            return Err(format!("{chat_id} is not a Captain on this device"));
        };
        self.captains.remove(position);
        Ok(())
    }

    /// The role `chat_id` plays in a run: a live Captain, crew of one, or
    /// nothing. A Captain archived either way — demoted or hidden — and
    /// every member of its crew are `RunRole::None`.
    pub fn run_role(&self, chat_id: &str) -> RunRole {
        if let Some(captain) = self
            .captains
            .iter()
            .find(|captain| captain.chat_id == chat_id && captain.live())
        {
            return RunRole::Captain {
                name: captain.name.clone(),
            };
        }
        if let Some(captain) = self.captain_of_crew(chat_id) {
            let meta = captain.crew_meta.get(chat_id).cloned().unwrap_or_default();
            return RunRole::Crew {
                captain_id: captain.chat_id.clone(),
                captain_name: captain.name.clone(),
                owns: meta.owns,
                worktree: meta.worktree,
            };
        }
        RunRole::None
    }
}

/// `<deepseek_home>/captains` — the root every Captain Drive lives under.
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
/// validates exactly these camelCase keys and reads `driveRoot` as the Drive
/// itself, so it names `drive_dir(deepseek_home, captain_id)` — the directory
/// `ensure_drive` creates — never its parent.
pub fn captains_env(registry: &Registry, deepseek_home: &Path, chat_id: &str) -> Option<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Stamp<'a> {
        role: &'a str,
        chat_id: &'a str,
        captain_id: &'a str,
        captain_name: &'a str,
        drive_root: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        owns: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        worktree: Option<String>,
    }

    let (role, captain_id, captain_name, owns, worktree) = match registry.run_role(chat_id) {
        RunRole::None => return None,
        RunRole::Captain { name } => ("captain", chat_id.to_string(), name, Vec::new(), None),
        RunRole::Crew {
            captain_id,
            captain_name,
            owns,
            worktree,
        } => ("crew", captain_id, captain_name, owns, worktree),
    };
    if valid_captain_row(chat_id, &captain_id, &captain_name).is_err() {
        return None;
    }
    Some(
        serde_json::to_string(&Stamp {
            role,
            chat_id,
            captain_id: &captain_id,
            captain_name: &captain_name,
            drive_root: drive_dir(deepseek_home, &captain_id)
                .to_string_lossy()
                .into_owned(),
            owns,
            worktree,
        })
        .expect("a captains stamp serialises"),
    )
}

/// The plugin's row-id rule (`config.js`): `[A-Za-z0-9_-]{1,128}`. A drive path
/// is only ever joined from an id that passes, so a row that arrived through
/// `upsert_chat`/import cannot escape `<deepseek_home>/captains`.
pub fn valid_captain_id(id: &str) -> Result<(), String> {
    if !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Ok(())
    } else {
        Err(format!(
            "invalid Captain id: {id:?} (allowed: 1-128 of A-Z, a-z, 0-9, '_' and '-')"
        ))
    }
}

// The plugin measures the same name with JS `String.length`
// (`config.js:23`, called unguarded from `index.js:54`), which counts UTF-16
// code units, so the limit is stated in that unit — not in `chars()` scalars.
const MAX_NAME_UNITS: usize = 80;

/// The plugin's name rule (`config.js`): 1-80 UTF-16 code units, no line
/// breaks. An astral character is one `chars()` scalar but two UTF-16 code
/// units, so the engine must count code units the way `String.length` does or
/// it stamps names the plugin throws on.
pub fn valid_captain_name(name: &str) -> Result<(), String> {
    if !name.is_empty()
        && name.encode_utf16().count() <= MAX_NAME_UNITS
        && !name.contains(['\r', '\n'])
    {
        Ok(())
    } else {
        Err(format!(
            "invalid Captain name: {name:?} (allowed: 1-{MAX_NAME_UNITS} UTF-16 code units, no line breaks)"
        ))
    }
}

/// Whether a row may be stamped at all: the plugin's `normalizeConfig`
/// throws on any of these, so a row that fails must run ordinary rather
/// than carry a stamp its consumer refuses.
pub fn valid_captain_row(
    chat_id: &str,
    captain_id: &str,
    captain_name: &str,
) -> Result<(), String> {
    valid_captain_id(chat_id)?;
    valid_captain_id(captain_id)?;
    valid_captain_name(captain_name)
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
            ..
        } => (captain_id, captain_name),
    };
    if valid_captain_row(chat_id, &captain_id, &captain_name).is_err() {
        return Ok(None);
    }
    let drive = drive_dir(deepseek_home, &captain_id);
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

// ── SUR-205: the digest's factual sections are the engine's, not the model's ──

/// The freshness marker ending a synthesized digest: the RFC 3339 second
/// the sections were written at, as an HTML comment so it never renders.
/// The next refresh reads it back to decide which reports are new.
const REFRESHED: &str = "<!-- refreshed ";

/// The engine-owned record of the last synthesis (`<drive>/state/` is a
/// folder the Drive tools refuse the model): the New cutoff a rewrite of
/// DIGEST.md cannot take away.
const CUTOFF_FILE: &str = "digest-refresh";

/// Every refresh the engine writes, process-wide. Two crew turns ending
/// together otherwise interleave their digest rename and sidecar write,
/// and the loser pairs an older digest with the winner's newer cutoff —
/// a report filed between the two never appears under New. The writes
/// are microseconds; the lock is never the bottleneck.
static REFRESH_LOCK: Mutex<()> = Mutex::new(());

/// One crew chat as the digest synthesis sees it: the roster id, the
/// workspace row's title, and whether the row is live — present and not
/// archived. A crew that is not live, or no longer on the roster at all,
/// leaves the digest at the next refresh: an archived or released row
/// never lingers because the model forgot to delete its bullet.
pub struct DigestCrew {
    pub chat_id: String,
    pub title: String,
    pub live: bool,
}

/// The facts one report file carries for the digest: the outcome word, the
/// filing second and the body's first line.
struct ReportFact {
    word: String,
    at: DateTime<Utc>,
    first_line: String,
}

/// Parse `<drive>/reports/<chat_id>.md` — the file `crew_reports::write_report`
/// writes. Absent, unreadable or headerless files are no fact at all; the
/// digest never fails over one malformed report.
fn report_fact(drive: &Path, chat_id: &str) -> Option<ReportFact> {
    let path = crate::crew_reports::report_path(drive, chat_id)?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut word = None;
    let mut at = None;
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("- Status: ") {
            word = Some(value.trim().to_owned());
        }
        if let Some(value) = line.strip_prefix("- At: ") {
            at = DateTime::parse_from_rfc3339(value.trim())
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc));
        }
    }
    let first_line = text
        .split("\n\n")
        .nth(1)
        .and_then(|body| body.lines().next())
        .unwrap_or_default()
        .trim()
        .chars()
        .take(120)
        .collect();
    Some(ReportFact {
        word: word?,
        at: at?,
        first_line,
    })
}

/// The bullet one crew earns in whichever section lists it: its title (the
/// short id when untitled), its short id, then its report's outcome — or
/// that it has filed nothing yet.
fn digest_bullet(crew: &DigestCrew, report: Option<&ReportFact>) -> String {
    let short: String = crew.chat_id.chars().take(8).collect();
    let title = if crew.title.is_empty() {
        format!("crew {short}")
    } else {
        crew.title.clone()
    };
    match report {
        None => format!("- {title} ({short}): no report yet"),
        Some(report) if report.first_line.is_empty() => format!(
            "- {title} ({short}): {} at {}",
            report.word,
            report.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        Some(report) => format!(
            "- {title} ({short}): {} at {} — {}",
            report.word,
            report.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            report.first_line
        ),
    }
}

/// The `## Needs you` section of `digest` exactly as written, heading line
/// included, or an empty string when the digest has none. Everything of the
/// digest outside this section is the engine's: the synthesis rewrites the
/// factual sections wholesale, so prose the model adds under them does not
/// survive — only Needs you is the model's.
fn needs_you_section(digest: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in digest.lines() {
        if line.starts_with("## ") || line.starts_with(REFRESHED) {
            if inside {
                break;
            }
            // The same heading the snapshot parser recognizes: the model's
            // capitalization is its own, and a refresh must not drop asks
            // `parse_needs_you` would have read.
            inside = line.trim().eq_ignore_ascii_case("## needs you");
        }
        if inside {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    kept.join("\n")
}

/// The previous freshness timestamp of `digest`, for the New cutoff: a
/// report is new when it was filed after the last synthesis.
fn previous_refresh(digest: &str) -> Option<DateTime<Utc>> {
    let start = digest.find(REFRESHED)? + REFRESHED.len();
    let end = digest[start..].find(" -->")? + start;
    DateTime::parse_from_rfc3339(digest[start..end].trim())
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Rewrite the factual sections of `<drive>/DIGEST.md` deterministically
/// (SUR-205) — `## New`, `## In progress`, `## Stalls` and the freshness
/// timestamp — from the crew facts the seam gathered and the reports on
/// disk, preserving the model-written `## Needs you` intact. The write is
/// atomic (temp file in the drive, then rename): the sections and their
/// timestamp move as one, so a reader never sees a new timestamp over old
/// bullets. Any section the model wrote outside `## Needs you` is replaced,
/// which is the point: the factual sections cannot drift from what the
/// roster, the rows and the reports actually say.
///
/// New holds reports filed since the previous synthesis; In progress the
/// live crew whose latest report completed (or who filed nothing yet);
/// Stalls the live crew whose latest report ended in error or abort. All
/// three skip a crew that is not live or no longer on the roster.
pub fn refresh_digest(
    drive: &Path,
    crew: &[DigestCrew],
    now: DateTime<Utc>,
) -> Result<(), String> {
    let _serialization = REFRESH_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let path = drive.join("DIGEST.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    // The cutoff is the engine-owned sidecar first: the model rewrites
    // DIGEST.md whole, and a write that drops the freshness comment must
    // not resurrect every report as new. The comment is the fallback for
    // a drive whose state directory is gone.
    let since = std::fs::read_to_string(drive.join("state").join(CUTOFF_FILE))
        .ok()
        .and_then(|text| {
            DateTime::parse_from_rfc3339(text.trim())
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc))
        })
        .or_else(|| previous_refresh(&existing));
    let mut new_items: Vec<String> = Vec::new();
    let mut in_progress: Vec<String> = Vec::new();
    let mut stalls: Vec<String> = Vec::new();
    for member in crew.iter().filter(|member| member.live) {
        let report = report_fact(drive, &member.chat_id);
        if let Some(fact) = report.as_ref()
            && since.is_none_or(|cutoff| fact.at > cutoff)
        {
            new_items.push(digest_bullet(member, report.as_ref()));
        }
        match report.as_ref().map(|fact| fact.word.as_str()) {
            Some("error") | Some("aborted") => stalls.push(digest_bullet(member, report.as_ref())),
            _ => in_progress.push(digest_bullet(member, report.as_ref())),
        }
    }
    let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut out = needs_you_section(&existing);
    out.push_str("\n\n## New\n");
    for line in &new_items {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\n## In progress\n");
    for line in &in_progress {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\n## Stalls\n");
    for line in &stalls {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("\n{REFRESHED}{stamp} -->\n"));
    let mut temp = tempfile::NamedTempFile::new_in(drive)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    temp.write_all(out.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    temp.persist(&path)
        .map_err(|error| format!("cannot write {}: {}", path.display(), error.error))?;
    // The sidecar follows the digest, never leads it: a crash between the
    // two re-lists a report as new — noise — rather than losing it from
    // New before it was ever listed. Best effort: a failed sidecar write
    // falls back to the digest's own marker, which this write just set.
    let state = drive.join("state");
    if std::fs::create_dir_all(&state).is_ok() {
        let _ = std::fs::write(state.join(CUTOFF_FILE), format!("{stamp}\n"));
    }
    Ok(())
}

/// The environment override for the bundled Captains plugin's directory.
pub const PLUGIN_DIR_ENV: &str = "ZERON_CAPTAINS_PLUGIN_DIR";

/// The bundled dsh-captains plugin directory, when this host ships one.
/// Resolution order: ZERON_CAPTAINS_PLUGIN_DIR — the explicit override, so
/// a value that holds no bundle is `None`, never a fallthrough; then the
/// layout this OS ships — macOS's `Contents/Resources`, the Windows
/// `beta/<version>` root above `engine`, or the Linux tarball root beside
/// the binary; then, in a debug build only, the repository checkout's
/// `plugins/dsh-captains`, because a compile-time path may still exist on
/// the machine that built a release binary and a release run must not
/// resolve it. `None` when none exists.
pub fn plugin_dir() -> Option<PathBuf> {
    plugin_dir_env(cfg!(debug_assertions))
}

/// [`plugin_dir`]'s inputs gathered — the override value, the running
/// executable, the target OS — with `debug` deciding whether the
/// repository checkout may resolve.
fn plugin_dir_env(debug: bool) -> Option<PathBuf> {
    // An empty override reads as unset; a missing executable path just
    // leaves the packaged candidate absent.
    let exe = std::env::current_exe().unwrap_or_default();
    plugin_dir_from(
        std::env::var_os(PLUGIN_DIR_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .as_deref(),
        &exe,
        std::env::consts::OS,
        debug,
    )
}

/// [`plugin_dir`] without the repository checkout: the bundle an install
/// laid down beside the executable, or the operator's explicit
/// `ZERON_CAPTAINS_PLUGIN_DIR`. Provisioning writes the bundle into the
/// `sdk` profile — a packaged app needs it done for it, and a machine
/// running a checkout already runs the plugin from source, so a prepare
/// on a dev or headless host must leave the profile alone.
pub(crate) fn packaged_plugin_dir() -> Option<PathBuf> {
    plugin_dir_env(false)
}

/// [`plugin_dir`]'s resolution with its inputs passed in — the override
/// value, the running executable's path, the target OS, and whether the
/// build carries debug assertions — so the tests drive it without mutating
/// the process environment.
///
/// The override is explicit: one that does not hold a bundle is `None`,
/// never a silent fallthrough — the operator named the plugin it wants, so
/// resolving to a different tree would run the wrong code. `os` selects
/// exactly one packaged layout beside `exe` — there is no
/// try-the-other-layouts fallback. `debug` gates the repository checkout:
/// its `env!("CARGO_MANIFEST_DIR")` path is baked in at compile time, so it
/// may still exist where a release binary was built and only a debug build
/// may resolve it.
fn plugin_dir_from(
    override_dir: Option<&Path>,
    exe: &Path,
    os: &str,
    debug: bool,
) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        return holds_plugin_bundle(dir).then(|| dir.to_path_buf());
    }
    // `os` names the one layout this platform ships: the app bundle's
    // `Contents/Resources` on macOS, the `beta/<version>` root above
    // `engine` on Windows, the tarball root beside the binary elsewhere. A
    // missing parent only means the candidate is absent — the lookup still
    // ends at the repo arm.
    let grandparent = exe.parent().and_then(Path::parent);
    let packaged = match os {
        "macos" => grandparent.map(|dir| dir.join("Resources/dsh-plugins/dsh-captains")),
        "windows" => grandparent.map(|dir| dir.join("dsh-plugins/dsh-captains")),
        _ => exe.parent().map(|dir| dir.join("dsh-plugins/dsh-captains")),
    };
    if let Some(dir) = packaged.filter(|dir| holds_plugin_bundle(dir)) {
        return Some(dir);
    }
    if !debug {
        // `env!("CARGO_MANIFEST_DIR")` is baked in at compile time, so the
        // checkout may still exist on the machine that built a release
        // binary — a release run must never resolve it.
        return None;
    }
    // This crate is `<checkout>/crates/engine`, so the checkout's plugin
    // tree is two directories up.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/dsh-captains");
    holds_plugin_bundle(&repo).then_some(repo)
}

/// Whether `dir` holds a bundled dsh-captains plugin: both `package.json`
/// and `index.js` are files — `index.js` is the entry point the manifest's
/// `"main"` names, so a directory carrying only the manifest is not a
/// plugin. The lookup never returns a directory that fails this: a
/// half-staged tree is `None`, not a path into nothing.
fn holds_plugin_bundle(dir: &Path) -> bool {
    ["package.json", "index.js"]
        .iter()
        .all(|name| dir.join(name).is_file())
}

/// The package name the `sdk` profile must list — in `dsh.profile.bundles`
/// so dsh loads it, and in `dependencies` with a `link:` spec so the
/// profile's own `node_modules` resolves it.
const BUNDLE_NAME: &str = "zeron-dsh-captains";

/// The ownership marker written into a materialised copy of the bundle. It
/// records the hash of the copy this host provisioned, so an upgrade can tell
/// that disposable copy from a real directory a person installed and refresh
/// only the former.
const COPY_MARKER: &str = ".zeron-dsh-captains.copy.json";

/// The `sdk` profile template dsh's `initProfile` writes, byte for byte:
/// the user patch layer every bundle layer is applied under, and the
/// workspace file that makes the profile a plain hoisted pnpm project.
const PATCH_TEMPLATE: &str = "# Your patch layer for this dsh profile, applied after every bundle layer:\n# a top-level YAML array of loader patch entries (id-targeted config\n# overrides, disables, and insert lists; `!!js` expressions allowed).\n[]\n";
const WORKSPACE_TEMPLATE: &str = "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n";

/// What one provisioning attempt did to the `sdk` profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// No profile existed: it was created from the shipped template with the bundle.
    Created,
    /// An existing profile was missing the bundle: only that entry was added.
    Added,
    /// Already listed and resolving at the packaged bundle: nothing was written.
    Present,
    /// Left alone, with a message to surface: the file could not be parsed,
    /// a structural key had an unexpected shape, or a write failed.
    Refused(String),
}

/// Provision `<home>/profiles/sdk` — the profile the dsh runtime boots —
/// so it lists and resolves the shipped Captains bundle. It runs from the
/// prepare step every DeepSeek run passes, so a packaged app never needs a
/// promote-time install or pnpm and a wake or a promotion never depends on
/// a user having run a plugin command.
///
/// The profile is a real person's directory, so the rules are sedate
/// conservatism: create from the shipped template only when absent; edit
/// an existing manifest surgically — every byte it held stays, and only
/// the missing bundle entries are added; leave alone one that already
/// resolves the bundle; and refuse — reported, never silently skipped —
/// a manifest that cannot be parsed, carries a structural key of the
/// wrong shape, or will not take a write.
pub fn provision_profile(home: &Path, bundle: &Path) -> ProvisionOutcome {
    let bundle = match std::fs::canonicalize(bundle) {
        Ok(bundle) => bundle,
        Err(error) => {
            return ProvisionOutcome::Refused(format!(
                "the bundled plugin directory {} cannot be resolved: {error}",
                bundle.display()
            ));
        }
    };
    let profile = home.join("profiles").join("sdk");
    let manifest = profile.join("package.json");
    match std::fs::read_to_string(&manifest) {
        Ok(text) => complete_profile(&profile, &bundle, &text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_profile(&profile, &bundle)
        }
        Err(error) => ProvisionOutcome::Refused(format!(
            "cannot read {}: {error}",
            manifest.display()
        )),
    }
}

/// Rule 2: no profile exists — write the template dsh ships, byte for
/// byte, then link `node_modules/<bundle>` at the packaged bundle, or
/// materialise it where no link primitive exists. Bundles resolve from
/// the profile's `node_modules` first, so the link is what must never
/// dangle.
fn create_profile(profile: &Path, bundle: &Path) -> ProvisionOutcome {
    let node_modules = profile.join("node_modules");
    if let Err(error) = std::fs::create_dir_all(&node_modules) {
        return ProvisionOutcome::Refused(format!(
            "cannot create {}: {error}",
            node_modules.display()
        ));
    }
    for (name, contents) in [
        ("package.json", manifest_template(bundle)),
        ("cordis.patch.yml", PATCH_TEMPLATE.into()),
        ("pnpm-workspace.yaml", WORKSPACE_TEMPLATE.into()),
    ] {
        // Create-if-absent: a directory holding a person's own patch layer
        // or workspace file but no manifest is still theirs — the template
        // fills gaps, it does not overwrite them.
        if let Err(error) = write_starter(&profile.join(name), &contents) {
            return ProvisionOutcome::Refused(error);
        }
    }
    match ensure_link(profile, bundle) {
        Ok(()) => ProvisionOutcome::Created,
        Err(reason) => ProvisionOutcome::Refused(reason),
    }
}

/// Rules 1 and 3: a profile exists, so only what is missing is added —
/// the `dependencies` member and the `dsh.profile.bundles` element as
/// surgical text edits on the bytes it already holds (a re-serialisation
/// would rewrite a real person's file), and the `node_modules` entry,
/// re-pointed when it is a link that does not resolve to `bundle` and
/// refused when it is a real directory or file that is not the bundle.
/// The patch layer and the workspace file of an existing profile are
/// never touched.
fn complete_profile(profile: &Path, bundle: &Path, text: &str) -> ProvisionOutcome {
    let manifest_path = profile.join("package.json");
    let manifest: Value = match serde_json::from_str(text) {
        Ok(manifest) => manifest,
        Err(error) => {
            return ProvisionOutcome::Refused(format!(
                "{} cannot be parsed ({error}); fix or delete it",
                manifest_path.display()
            ));
        }
    };
    let refused_shape = |key: &str| {
        ProvisionOutcome::Refused(format!(
            "{key} in {} is not the shape dsh writes; fix or delete it",
            manifest_path.display()
        ))
    };
    if !manifest.is_object() {
        return refused_shape("the document");
    }
    let dependencies = manifest.get("dependencies");
    if dependencies.is_some_and(|value| !value.is_object()) {
        return refused_shape("\"dependencies\"");
    }
    let dsh = manifest.get("dsh");
    if dsh.is_some_and(|value| !value.is_object()) {
        return refused_shape("\"dsh\"");
    }
    let profile_section = dsh.and_then(|value| value.get("profile"));
    if profile_section.is_some_and(|value| !value.is_object()) {
        return refused_shape("\"dsh.profile\"");
    }
    let bundles = profile_section.and_then(|value| value.get("bundles"));
    if bundles.is_some_and(|value| !value.is_array()) {
        return refused_shape("\"dsh.profile.bundles\"");
    }
    let declared = dependencies
        .and_then(Value::as_object)
        .is_some_and(|deps| deps.contains_key(BUNDLE_NAME));
    let listed = bundles
        .and_then(Value::as_array)
        .is_some_and(|list| list.iter().any(|entry| entry.as_str() == Some(BUNDLE_NAME)));
    if declared && listed && link_resolves(profile, bundle) {
        return ProvisionOutcome::Present;
    }

    // The edits are byte ranges in `text`: serde_json decided what the
    // manifest means, the scanners below only find where its pieces sit.
    let bytes = text.as_bytes();
    // `manifest` is an object, so the first non-whitespace byte is '{'.
    let root_open = skip_ws(bytes, 0);
    let unlocated = |path: &Path| {
        ProvisionOutcome::Refused(format!(
            "{} could not be followed for a surgical edit; fix or delete it",
            path.display()
        ))
    };
    let mut edits: Vec<(usize, String)> = Vec::new();
    let spec = format!("\"{BUNDLE_NAME}\": {}", json!(format!("link:{}", bundle.display())));
    if !declared {
        if dependencies.is_some() {
            match at(bytes, locate(bytes, root_open, &["dependencies"]), b'{') {
                Some(open) => edits.push(member_insert(text, open, &spec)),
                None => return unlocated(&manifest_path),
            }
        } else {
            edits.push(member_insert(
                text,
                root_open,
                &format!("\"dependencies\": {{{spec}}}"),
            ));
        }
    }
    if !listed {
        if bundles.is_some() {
            match at(bytes, locate(bytes, root_open, &["dsh", "profile", "bundles"]), b'[') {
                Some(open) => {
                    edits.push(element_insert(text, open, &format!("\"{BUNDLE_NAME}\"")))
                }
                None => return unlocated(&manifest_path),
            }
        } else if profile_section.is_some() {
            match at(bytes, locate(bytes, root_open, &["dsh", "profile"]), b'{') {
                Some(open) => edits.push(member_insert(
                    text,
                    open,
                    &format!("\"bundles\": [\"{BUNDLE_NAME}\"]"),
                )),
                None => return unlocated(&manifest_path),
            }
        } else if dsh.is_some() {
            match at(bytes, locate(bytes, root_open, &["dsh"]), b'{') {
                Some(open) => edits.push(member_insert(
                    text,
                    open,
                    &format!("\"profile\": {{\"bundles\": [\"{BUNDLE_NAME}\"]}}"),
                )),
                None => return unlocated(&manifest_path),
            }
        } else {
            edits.push(member_insert(
                text,
                root_open,
                &format!("\"dsh\": {{\"profile\": {{\"bundles\": [\"{BUNDLE_NAME}\"]}}}}"),
            ));
        }
    }
    if !edits.is_empty() {
        edits.sort_by_key(|edit| edit.0);
        let mut updated = String::with_capacity(text.len() + 128);
        let mut rest = 0usize;
        let mut i = 0;
        while i < edits.len() {
            let at = edits[i].0;
            updated.push_str(&text[rest..at]);
            // Inserts landing at the same offset — two missing root
            // members into a `{}` manifest is the case that ships — are
            // one insertion with commas between the members: applied
            // independently, the second drops the separator and the
            // write does not parse.
            let mut merged = edits[i].1.clone();
            i += 1;
            while i < edits.len() && edits[i].0 == at {
                if !merged.ends_with(',') {
                    merged.push(',');
                }
                merged.push_str(&edits[i].1);
                i += 1;
            }
            updated.push_str(&merged);
            rest = at;
        }
        updated.push_str(&text[rest..]);
        // The write happens only when what would land still parses as a
        // JSON object: a manifest that cannot be completed safely is
        // refused, never written.
        match serde_json::from_str::<Value>(&updated) {
            Ok(value) if value.is_object() => {}
            _ => {
                return ProvisionOutcome::Refused(format!(
                    "{} could not be completed safely; fix or delete it",
                    manifest_path.display()
                ));
            }
        }
        if let Err(error) = std::fs::write(&manifest_path, updated) {
            return ProvisionOutcome::Refused(format!(
                "cannot write {}: {error}",
                manifest_path.display()
            ));
        }
    }
    match ensure_link(profile, bundle) {
        Ok(()) => ProvisionOutcome::Added,
        Err(reason) => ProvisionOutcome::Refused(reason),
    }
}

/// Whether `profile/node_modules/<bundle>` already *is* the bundle — a
/// read, never a write: a link that canonicalises to `bundle`, or a real
/// directory whose `package.json` names it. A real directory without the
/// copy marker is a person's hoisted install and counts as present without
/// any further comparison, because provisioning never rewrites it. A marked
/// copy counts only while both its marker and its bytes still match the
/// packaged bundle; otherwise it is this host's own stale copy and can be
/// refreshed.
fn link_resolves(profile: &Path, bundle: &Path) -> bool {
    let entry = profile.join("node_modules").join(BUNDLE_NAME);
    if std::fs::canonicalize(&entry).is_ok_and(|target| target == bundle) {
        return true;
    }
    // `symlink_metadata` reports the entry's own type: `is_dir` here is a
    // real directory, never a link to one.
    if !(std::fs::symlink_metadata(&entry).is_ok_and(|metadata| metadata.is_dir())
        && names_bundle(&entry))
    {
        return false;
    }
    let Some(recorded) = copy_marker_hash(&entry) else {
        return true;
    };
    let Ok(packaged) = bundle_hash(bundle) else {
        return false;
    };
    recorded == packaged && bundle_hash(&entry).is_ok_and(|copy| copy == recorded)
}

/// Whether the real directory at `entry` is the bundle itself: its
/// `package.json` parses and carries `"name": "<bundle>"` — the shape a
/// hoisted install or a materialised copy leaves.
fn names_bundle(entry: &Path) -> bool {
    std::fs::read_to_string(entry.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|manifest| manifest["name"].as_str().map(str::to_owned))
        .is_some_and(|name| name == BUNDLE_NAME)
}

/// The hash recorded in the copy marker at `entry`, or `None` when there is
/// no marker this host recognises. Requiring the bundle name and schema
/// version keeps an unrelated file of the same name from making a person's
/// directory look disposable.
fn copy_marker_hash(entry: &Path) -> Option<String> {
    let text = std::fs::read_to_string(entry.join(COPY_MARKER)).ok()?;
    let marker: Value = serde_json::from_str(&text).ok()?;
    if marker.get("version").and_then(Value::as_u64) != Some(1)
        || marker.get("bundle").and_then(Value::as_str) != Some(BUNDLE_NAME)
    {
        return None;
    }
    marker
        .get("bundleHash")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A stable hash of the files `copy_bundle` materialises: relative paths in
/// sorted order plus each file's length and bytes. Top-level `node_modules`,
/// `test` and the copy marker are outside the bundle exactly as they are in
/// the copy; links are not followed. The hash is platform-stable because
/// path components join with `/` before hashing.
fn bundle_hash(root: &Path) -> std::io::Result<String> {
    let mut files = Vec::new();
    collect_bundle_files(root, root, true, &mut files)?;
    files.sort();
    let mut hasher = sha2::Sha256::new();
    for file in files {
        let path = file
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        let bytes = std::fs::read(root.join(&file))?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Collect the regular files `copy_bundle` would carry below `dir`, with
/// paths relative to `root`. `read_dir` order is platform-dependent, so the
/// caller sorts the collected paths before hashing them.
fn collect_bundle_files(
    dir: &Path,
    root: &Path,
    top: bool,
    files: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if top
            && matches!(
                entry.file_name().to_str(),
                Some("node_modules" | "test") | Some(COPY_MARKER)
            )
        {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_bundle_files(&entry.path(), root, false, files)?;
        } else if file_type.is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("a recursive entry is below its root")
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

/// Link `profile/node_modules/<bundle>` at `bundle`: created when absent,
/// re-pointed when it is a link resolving anywhere else — and only the
/// link itself is ever removed, never another `node_modules` name and
/// never the tree a link points at. A real directory or file at that
/// name is a person's package, not ours: one that is already the bundle
/// is `link_resolves`' present case, and anything else refuses rather
/// than being deleted or overwritten. The one exception is a directory
/// carrying this provisioning copy's marker: that marker is written only
/// into a copy this host made, so the tree is disposable and an upgrade
/// refreshes it in place. Where no link primitive exists — Windows
/// without Developer Mode cannot make a directory symlink — the bundle
/// is materialised instead.
fn ensure_link(profile: &Path, bundle: &Path) -> Result<(), String> {
    ensure_link_with(profile, bundle, link_dir)
}

/// `ensure_link` with the platform link primitive passed in, so the copy
/// fallback is exercised on every OS rather than only on a host whose
/// symlink creation really fails.
fn ensure_link_with(
    profile: &Path,
    bundle: &Path,
    linker: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    if link_resolves(profile, bundle) {
        return Ok(());
    }
    let node_modules = profile.join("node_modules");
    let link = node_modules.join(BUNDLE_NAME);
    std::fs::create_dir_all(&node_modules)
        .map_err(|error| format!("cannot create {}: {error}", node_modules.display()))?;
    // Refreshing a copy rewrites the entry through sibling staging trees.
    // Chat starts can provision the same profile concurrently, so those
    // rewrites are serialised across threads and processes before the
    // entry is inspected; the second prepare then sees the first prepare's
    // completed copy instead of deleting its staging tree mid-copy.
    let lock_path = node_modules.join(format!(".{BUNDLE_NAME}.prepare.lock"));
    let provisioning_lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("cannot open {}: {error}", lock_path.display()))?;
    provisioning_lock
        .lock()
        .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;
    if link_resolves(profile, bundle) {
        return Ok(());
    }
    match std::fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            remove_link(&link)
                .map_err(|error| format!("cannot re-point {}: {error}", link.display()))?;
        }
        Ok(metadata) if metadata.is_dir() && copy_marker_hash(&link).is_some() => {
            return materialise_bundle(bundle, &link).map_err(|error| {
                format!(
                    "cannot refresh the materialised {BUNDLE_NAME} copy at {}: {error}",
                    link.display()
                )
            });
        }
        Ok(_) => {
            return Err(format!(
                "{} is not the {BUNDLE_NAME} bundle; fix or remove it",
                link.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!("cannot inspect {}: {error}", link.display()));
        }
    }
    match linker(bundle, &link) {
        Ok(()) => Ok(()),
        Err(link_error) => materialise_bundle(bundle, &link).map_err(|copy_error| {
            format!(
                "cannot link {} to {} ({link_error}) or materialise it there ({copy_error})",
                link.display(),
                bundle.display()
            )
        }),
    }
}

/// Remove `link`, which `symlink_metadata` reported as a symlink: unix
/// unlinks it whatever it points at; Windows splits link removal by the
/// target's shape — a directory link removes as a directory and anything
/// else as a file — so the directory shape is tried first. The link's
/// target is never touched.
#[cfg(windows)]
fn remove_link(link: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link))
}

/// Remove `link`, which `symlink_metadata` reported as a symlink: an
/// unlink removes it whatever it points at, and the target is untouched.
#[cfg(not(windows))]
fn remove_link(link: &Path) -> std::io::Result<()> {
    std::fs::remove_file(link)
}

/// Materialise the bundle at `link` — the fallback for hosts with no
/// usable link primitive — as a real directory holding the files the
/// bundle ships plus the marker that makes the copy refreshable. The copy
/// stages beside `link`, so a copy failure never leaves a half-shipped
/// bundle at the entry. On a refresh the old copy moves aside only after
/// the new one is complete, moves back if the install rename fails, and is
/// removed once the new copy is in place.
fn materialise_bundle(bundle: &Path, link: &Path) -> std::io::Result<()> {
    let staging = link.with_file_name(format!(".{BUNDLE_NAME}.materialising"));
    let retiring = link.with_file_name(format!(".{BUNDLE_NAME}.retiring"));
    let result = (|| {
        remove_own_temp_tree(&staging)?;
        std::fs::create_dir(&staging)?;
        copy_bundle(bundle, &staging, true)?;
        write_copy_marker(&staging)?;
        remove_own_temp_tree(&retiring)?;
        match std::fs::symlink_metadata(link) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::rename(&staging, link)
            }
            Err(error) => Err(error),
            Ok(_) => {
                std::fs::rename(link, &retiring)?;
                match std::fs::rename(&staging, link) {
                    Ok(()) => remove_own_temp_tree(&retiring),
                    Err(error) => match std::fs::rename(&retiring, link) {
                        Ok(()) => Err(error),
                        Err(rollback) => Err(std::io::Error::other(format!(
                            "cannot install the refreshed copy ({error}); restoring the old copy failed too ({rollback})"
                        ))),
                    },
                }
            }
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Remove one of `materialise_bundle`'s own temporary trees. The names are
/// private to provisioning, so a stale one is safe to delete: it can only be
/// the interrupted copy it was staged as.
fn remove_own_temp_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Write the ownership marker for a materialised copy. The hash is taken
/// from the copy itself after materialisation, so it describes the exact
/// bytes a later prepare compares with the packaged bundle.
fn write_copy_marker(copy: &Path) -> std::io::Result<()> {
    let marker = json!({
        "version": 1,
        "bundle": BUNDLE_NAME,
        "bundleHash": bundle_hash(copy)?,
    });
    std::fs::write(copy.join(COPY_MARKER), format!("{marker}\n"))
}

/// Copy the files the bundle ships from `from` into `to`, recursively:
/// the manifest, the entry point, the patch layer and `src` — which
/// `index.js` imports — with the packager's own `node_modules` and `test`
/// left out at the top level. The copy marker is metadata for the copy, not
/// bundle payload, and a link inside the bundle is not followed.
fn copy_bundle(from: &Path, to: &Path, top: bool) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if top
            && matches!(
                entry.file_name().to_str(),
                Some("node_modules" | "test") | Some(COPY_MARKER)
            )
        {
            continue;
        }
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir(&target)?;
            copy_bundle(&entry.path(), &target, false)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// `symlink_dir` per platform; a failure is a refusal, never a panic.
#[cfg(unix)]
fn link_dir(bundle: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(bundle, link)
}

/// `symlink_dir` per platform; a failure is a refusal, never a panic.
#[cfg(windows)]
fn link_dir(bundle: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(bundle, link)
}

/// `symlink_dir` per platform; a failure is a refusal, never a panic.
#[cfg(not(any(unix, windows)))]
fn link_dir(_bundle: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no directory symlink primitive on this OS",
    ))
}

/// The `package.json` dsh's `initProfile` writes for a profile that takes
/// the Captains bundle: the shipped bundles plus ours, `link:`-ed in.
fn manifest_template(bundle: &Path) -> String {
    format!(
        r#"{{
  "name": "dsh-profile-sdk",
  "private": true,
  "dependencies": {{
    "zeron-dsh-captains": {spec}
  }},
  "dsh": {{
    "profile": {{
      "bundles": [
        "@deepseek-ai/dsh-base",
        "@deepseek-ai/dsh-sdk-app",
        "zeron-dsh-captains"
      ]
    }}
  }}
}}
"#,
        spec = json!(format!("link:{}", bundle.display()))
    )
}

// The scans below locate structure in manifest bytes serde_json already
// validated — they never decide JSON-ness, so strings are the only place
// braces and brackets can hide.

fn skip_ws(b: &[u8], mut p: usize) -> usize {
    while b
        .get(p)
        .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
    {
        p += 1;
    }
    p
}

/// The offset just past the closing quote of the string at `p`.
fn string_end(b: &[u8], mut p: usize) -> usize {
    p += 1;
    while let Some(&c) = b.get(p) {
        match c {
            b'\\' => p += 2,
            b'"' => return p + 1,
            _ => p += 1,
        }
    }
    p
}

/// The `[start, end)` span of the value at or just after `p`.
fn value_span(b: &[u8], p: usize) -> (usize, usize) {
    let start = skip_ws(b, p);
    match b.get(start) {
        Some(b'"') => (start, string_end(b, start)),
        Some(&open @ (b'{' | b'[')) => {
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 1usize;
            let mut i = start + 1;
            while i < b.len() {
                match b[i] {
                    b'"' => i = string_end(b, i),
                    c if c == open => {
                        depth += 1;
                        i += 1;
                    }
                    c if c == close => {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            return (start, i);
                        }
                    }
                    _ => i += 1,
                }
            }
            (start, b.len())
        }
        Some(_) => {
            let mut i = start;
            while b.get(i).is_some_and(|c| {
                !matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']')
            }) {
                i += 1;
            }
            (start, i)
        }
        None => (start, start),
    }
}

/// One member of a scanned object: the key text, where the key starts,
/// and its value's span.
struct Member {
    key: String,
    key_start: usize,
    value: (usize, usize),
}

/// The members of the object whose `{` is at `open`, in order.
fn object_members(b: &[u8], open: usize) -> Vec<Member> {
    let mut members = Vec::new();
    let mut p = skip_ws(b, open + 1);
    while b.get(p) == Some(&b'"') {
        let key_start = p;
        let key_end = string_end(b, p);
        let key = String::from_utf8_lossy(&b[key_start + 1..key_end - 1]).into_owned();
        let value = value_span(b, skip_ws(b, skip_ws(b, key_end) + 1));
        members.push(Member {
            key,
            key_start,
            value,
        });
        let after = skip_ws(b, value.1);
        match b.get(after) {
            Some(&b',') => p = skip_ws(b, after + 1),
            _ => return members,
        }
    }
    members
}

/// The elements of the array whose `[` is at `open`: (ws_start, start,
/// end) each, where `b[ws_start..start]` is the whitespace run — newline
/// and indent included — the element sits after.
fn array_elements(b: &[u8], open: usize) -> Vec<(usize, usize, usize)> {
    let mut elements = Vec::new();
    let mut ws_start = open + 1;
    let mut p = skip_ws(b, ws_start);
    while b.get(p) != Some(&b']') && p < b.len() {
        let (start, end) = value_span(b, p);
        elements.push((ws_start, start, end));
        let after = skip_ws(b, end);
        match b.get(after) {
            Some(&b',') => {
                ws_start = after + 1;
                p = skip_ws(b, ws_start);
            }
            _ => break,
        }
    }
    elements
}

/// The opening-brace offset of the value the member chain `path` names,
/// or None when a link in the chain can't be followed in the raw bytes.
fn locate(b: &[u8], root_open: usize, path: &[&str]) -> Option<usize> {
    let mut open = root_open;
    for key in path {
        open = object_members(b, open)
            .into_iter()
            .find(|member| member.key == *key)?
            .value
            .0;
    }
    Some(open)
}

/// The located `open`, but only when it lands on `brace`: the shape
/// serde_json decided and the bytes must agree — a repeated key or an
/// escaped name makes them disagree, and then the edit is refused rather
/// than guessed.
fn at(b: &[u8], open: Option<usize>, brace: u8) -> Option<usize> {
    open.filter(|&p| b.get(p) == Some(&brace))
}

/// The edit that puts `member` inside the object opened at `open`, right
/// after the `{`: it lands in the first existing member's whitespace —
/// newline and indent included — and takes a comma when members already
/// follow.
fn member_insert(text: &str, open: usize, member: &str) -> (usize, String) {
    match object_members(text.as_bytes(), open).first() {
        Some(first) => (
            open + 1,
            format!("{}{},", &text[open + 1..first.key_start], member),
        ),
        None => (open + 1, member.to_string()),
    }
}

/// The edit that puts `element` inside the array opened at `open`, right
/// before the `]`: a comma precedes it when elements already exist, and
/// it keeps the last element's own whitespace and indent.
fn element_insert(text: &str, open: usize, element: &str) -> (usize, String) {
    match array_elements(text.as_bytes(), open).last() {
        Some(&(ws_start, start, end)) => (
            end,
            format!(",{}{}", &text[ws_start..start], element),
        ),
        None => (open + 1, element.to_string()),
    }
}

/// Serialises every `CaptainsStore` mutation process-wide: two independent
/// stores over the same `captains.json` still take turns under it.
static MUTATION: Mutex<()> = Mutex::new(());

/// What `snapshot` serves: the last published registry, and whether it is
/// only the empty stand-in a failed `Registry::load` degrades to. The flag
/// shares the registry's lock so a degraded read's repair retry serialises
/// with a `mutate` publish — without that, a retry holding an older revision
/// could overwrite a just-saved one.
struct Published {
    registry: Registry,
    /// The last `Registry::load` failed: `registry` is the empty stand-in,
    /// and the next `snapshot` retries the file before answering.
    degraded: bool,
}

/// The engine's single read/modify/write path over `captains.json`. Reads
/// serve the last published registry; while the store is degraded from a
/// failed load, the next read re-loads the file under the snapshot lock and
/// adopts it if it now parses. Every mutation takes the process-wide
/// `MUTATION` lock, loads the file fresh under it, applies the same rule
/// checks as `Registry`, saves atomically, and only then publishes the new
/// snapshot — so two racing assignments can never both win. A rule error or
/// a save error leaves the file and the snapshot as they were.
pub struct CaptainsStore {
    data_dir: PathBuf,
    /// The last published registry; replaced only after a save succeeds, or
    /// by a degraded read that finds the file repaired.
    snapshot: Mutex<Published>,
    /// Writes that saved and published — the tests pin "one update per
    /// committed change", so a retry that changes nothing must not count.
    #[cfg(test)]
    commits: std::sync::atomic::AtomicUsize,
}

impl CaptainsStore {
    /// Open the store over `data_dir`, loading `captains.json` once. A file
    /// that won't load degrades rather than stopping the engine: the store
    /// starts on an empty roster, and the degrade is no latch on either path
    /// — the next `snapshot` retries the file and adopts it when it parses,
    /// and a mutation reloads the file under `MUTATION`, so a repaired file
    /// is written normally. A mutation attempted while the file is still
    /// unreadable is refused with the parse error (which names the path).
    pub fn open(data_dir: PathBuf) -> CaptainsStore {
        let snapshot = match Registry::load(&data_dir) {
            Ok(registry) => Published {
                registry,
                degraded: false,
            },
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    data_dir = %data_dir.display(),
                    "captains registry load failed; starting with an empty roster"
                );
                Published {
                    registry: Registry::default(),
                    degraded: true,
                }
            }
        };
        CaptainsStore {
            data_dir,
            snapshot: Mutex::new(snapshot),
            #[cfg(test)]
            commits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// A clone of this store's last published registry (each store keeps its
    /// own; only the write lock is process-wide). While the store is degraded
    /// the call first re-loads the file under the same lock — an externally
    /// repaired file is adopted here, with no mutation and no reopen; a file
    /// that still won't parse keeps the empty stand-in serving.
    pub fn snapshot(&self) -> Registry {
        let mut published = self
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if published.degraded {
            // Held across the load so this retry can never publish an older
            // revision over a `mutate` write.
            if let Ok(registry) = Registry::load(&self.data_dir) {
                *published = Published {
                    registry,
                    degraded: false,
                };
            }
        }
        published.registry.clone()
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
            .unwrap_or_else(PoisonError::into_inner) = Published {
            registry: fresh,
            degraded: false,
        };
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

    /// Hide `chat_id`'s Captain without demoting it (see
    /// `Registry::archive`). `live` decides — under `MUTATION`, on the
    /// state being committed — which crew chats the archive can move; the
    /// marker's ids come back for the caller to archive. A retried archive
    /// returns the kept marker again and commits nothing.
    pub fn archive(
        &self,
        chat_id: &str,
        live: impl Fn(&str) -> bool,
    ) -> Result<Vec<String>, String> {
        let mut moved = Vec::new();
        self.mutate(|registry| {
            let (ids, changed) = registry.archive(chat_id, &live)?;
            moved = ids;
            Ok(changed)
        })?;
        Ok(moved)
    }

    /// The ids an `UnarchiveCaptain` would wake (see
    /// `Registry::unarchive_pending`): read off the last committed
    /// snapshot — a read, so `MUTATION` is never taken for it.
    pub fn unarchive_pending(&self, chat_id: &str) -> Result<Vec<String>, String> {
        self.snapshot().unarchive_pending(chat_id)
    }

    /// Spend a hidden Captain's marker (see `Registry::unarchive`): the
    /// finish step the caller runs only after the wake writes land — the
    /// flag clears and the marker's ids come back. A Captain that is not
    /// hidden and has no marker spends nothing and commits nothing.
    pub fn unarchive(&self, chat_id: &str) -> Result<Vec<String>, String> {
        let mut woke = Vec::new();
        self.mutate(|registry| {
            let (ids, changed) = registry.unarchive(chat_id)?;
            woke = ids;
            Ok(changed)
        })?;
        Ok(woke)
    }

    /// Drop `chat_id`'s record (see `Registry::delete`): the chats and the
    /// Drive folder are the caller's to keep.
    pub fn delete(&self, chat_id: &str) -> Result<(), String> {
        self.mutate(|registry| registry.delete(chat_id).map(|()| true))
    }
}

// ── the Captain RPC methods ─────────────────────────────────────────────

/// Every method [`CaptainsRpc`] answers. [`handles`] routes from this table and
/// the engine's tests pin each member to `zeron_rpc::local_only`, so a method
/// added to the roster surface cannot be relay-reachable by default.
pub const SURFACE: &[&str] = &[
    methods::CAPTAINS,
    methods::PROMOTE_CAPTAIN,
    methods::DEMOTE_CAPTAIN,
    methods::ADD_CREW,
    methods::REMOVE_CREW,
    methods::ARCHIVE_CAPTAIN,
    methods::UNARCHIVE_CAPTAIN,
    methods::DELETE_CAPTAIN,
];

/// Whether this RPC serves `method`.
pub fn handles(method: &str) -> bool {
    SURFACE.contains(&method)
}

/// One chat row's facts the roster rules read before the registry is touched.
#[derive(Debug, Clone, Default)]
pub struct ChatRow {
    pub space_id: Option<String>,
    pub parent_chat_id: Option<String>,
    pub device_id: String,
    /// The chat's live turn, from the sessions engine.
    pub turn_in_flight: bool,
    /// The person's own archival of the chat, which the cascade must
    /// never overturn.
    pub archived: bool,
}

/// The host's chat rows, as the roster rules read them.
pub trait Chats {
    fn chat(&self, chat_id: &str) -> Option<ChatRow>;
    /// The cascade's reach into the workspace (SUR-94): archive or
    /// unarchive a chat's row, with the workspace's error as text.
    fn set_chat_archived(&self, chat_id: &str, archived: bool) -> Result<(), String>;
}

/// The row gate every roster change passes first: the chat exists, lives
/// on this device, and has no turn in flight a roster change would cut
/// across (the sessions engine's `turn_in_flight` — the engine keeps no
/// other busy flag). One decision with two callers — `row` maps the refusal
/// into `RpcError::BadParams`, `committed_row` keeps the `String` the
/// commit closure propagates.
fn row_gate(chat_id: &str, row: Option<ChatRow>, device_id: &str) -> Result<ChatRow, String> {
    let Some(row) = row else {
        return Err(format!("no such chat: {chat_id}"));
    };
    if row.device_id != device_id {
        return Err(format!("{chat_id} belongs to another device"));
    }
    if row.turn_in_flight {
        return Err(format!("{chat_id} has a turn in flight"));
    }
    Ok(row)
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddCrewParams {
    captain_id: String,
    chat_id: String,
    owns: Option<Vec<String>>,
    #[serde(default)]
    worktree: Option<String>,
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
    user_archived: bool,
    crew: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    crew_meta: BTreeMap<String, CrewMeta>,
    drive_path: String,
}

/// The Captain methods, over the store and the host's chat rows.
pub struct CaptainsRpc<'a> {
    pub store: &'a CaptainsStore,
    pub chats: &'a dyn Chats,
    /// This engine's own device id, from `DocHost::device_id()`.
    pub device_id: &'a str,
    /// The DeepSeek home the roster projects each Captain's Drive path from.
    pub deepseek_home: &'a Path,
}

impl CaptainsRpc<'_> {
    /// Answer one of the methods. Every successful reply is the same
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
                let params: AddCrewParams = zeron_rpc::parse_params(params.clone())?;
                self.add_crew(&params)
            }
            methods::REMOVE_CREW => {
                let params: CrewParams = zeron_rpc::parse_params(params.clone())?;
                self.remove_crew(&params)
            }
            methods::ARCHIVE_CAPTAIN => {
                let params: DemoteParams = zeron_rpc::parse_params(params.clone())?;
                self.archive(&params)
            }
            methods::UNARCHIVE_CAPTAIN => {
                let params: DemoteParams = zeron_rpc::parse_params(params.clone())?;
                self.unarchive(&params)
            }
            methods::DELETE_CAPTAIN => {
                let params: DemoteParams = zeron_rpc::parse_params(params.clone())?;
                self.delete(&params)
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
                user_archived: captain.user_archived,
                crew: captain.crew.clone(),
                crew_meta: captain.crew_meta.clone(),
                drive_path: drive_dir(self.deepseek_home, &captain.chat_id)
                    .to_string_lossy()
                    .into_owned(),
            })
            .collect();
        json!({ "captains": captains })
    }

    /// A fast pre-check only, read outside `MUTATION`: it produces the early
    /// refusals, and the commit closure re-reads the same facts under the
    /// lock via `committed_row` — which is the decision. The gate itself is
    /// [`row_gate`].
    fn row(&self, chat_id: &str) -> Result<ChatRow, RpcError> {
        row_gate(chat_id, self.chats.chat(chat_id), self.device_id)
            .map_err(RpcError::BadParams)
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
    /// The residual: an `upsert_chat`, `set_chat_archived` or `delete_chat`
    /// that lands between this under-lock re-read and the atomic rename is
    /// still unobserved — a window now bounded by the save+rename inside the
    /// same uninterrupted critical section, not by the whole pre-check
    /// window. A turn that starts between the under-lock re-read and the
    /// rename samples the previous role and is corrected on its next turn.
    /// A deleted row still leaves its registry record, because the chat
    /// store does not cascade and this read cannot see a delete that lands
    /// after it; such a record is repaired explicitly (SUR-189) — the
    /// roster keeps showing it, `DeleteCaptain` drops a Captain record
    /// without its row, and `RemoveCrew` drops a crew entry whose member
    /// row is gone.
    fn committed_row(&self, chat_id: &str) -> Result<ChatRow, String> {
        row_gate(chat_id, self.chats.chat(chat_id), self.device_id)
    }

    /// `AddCrew`'s under-lock gate: both rows re-read under `MUTATION`, with
    /// the add-time project rule applied to the state being committed.
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
                valid_captain_name(&params.name)?;
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
                if captain.user_archived {
                    return Err(format!(
                        "a hidden Captain cannot be demoted: {}",
                        params.chat_id
                    ));
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
    fn add_crew(&self, params: &AddCrewParams) -> Result<Value, RpcError> {
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
                let existing = registry
                    .captain_of_crew(&params.chat_id)
                    .filter(|owner| owner.chat_id == params.captain_id)
                    .and_then(|owner| owner.crew_meta.get(&params.chat_id))
                    .cloned()
                    .unwrap_or_default();
                let meta = CrewMeta {
                    owns: params.owns.clone().unwrap_or(existing.owns),
                    worktree: params.worktree.clone().or(existing.worktree),
                };
                // A retried add commits nothing; the same call carrying
                // ownership metadata updates that metadata. Crewing for
                // ANOTHER live Captain is the registry's "at most one crew"
                // refusal.
                if let Some(owner) = registry.captain_of_crew(&params.chat_id)
                    && owner.chat_id == params.captain_id
                {
                    let current = owner
                        .crew_meta
                        .get(&params.chat_id)
                        .cloned()
                        .unwrap_or_default();
                    return if current == meta {
                        Ok(false)
                    } else {
                        registry
                            .add_crew_with_metadata(&params.captain_id, &params.chat_id, meta)
                            .map(|()| true)
                    };
                }
                registry
                    .add_crew_with_metadata(&params.captain_id, &params.chat_id, meta)
                    .map(|()| true)
            })
            .map_err(RpcError::BadParams)?;
        Ok(self.roster())
    }

    /// `RemoveCrew`: removing a member that isn't there is a committed
    /// no-op; a Captain that isn't live has no crew to change. The project
    /// rule gates `AddCrew`, not this call — a member whose chat row moved
    /// to another project after the add must still be releasable, so the
    /// gate here is the row gate only. A member row that is gone is the
    /// record a delete in the commit window leaves (SUR-189), not a bad
    /// argument — the roster keeps it and this call is the repair, so only
    /// a row that still exists must pass the gate; the Captain's own row is
    /// still required, and `DeleteCaptain` repairs a missing Captain.
    fn remove_crew(&self, params: &CrewParams) -> Result<Value, RpcError> {
        self.row(&params.captain_id)?;
        if let Some(row) = self.chats.chat(&params.chat_id) {
            row_gate(&params.chat_id, Some(row), self.device_id).map_err(RpcError::BadParams)?;
        }
        self.store
            .mutate(|registry| {
                self.committed_row(&params.captain_id)?;
                if let Some(row) = self.chats.chat(&params.chat_id) {
                    row_gate(&params.chat_id, Some(row), self.device_id)?;
                }
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

    /// `ArchiveCaptain` (SUR-94): the registry flag and the marker commit
    /// first — `live`, read under the store lock, names the crew chats the
    /// workspace can still archive — then each named chat is archived. A
    /// retry after a partial failure finds `userArchived` already set and
    /// moves nothing twice; a demoted Captain is refused, never hidden.
    /// These manage the record, not the row: no row gate stands in front.
    fn archive(&self, params: &DemoteParams) -> Result<Value, RpcError> {
        let moved = self
            .store
            .archive(&params.chat_id, |chat_id| {
                self.chats.chat(chat_id).is_some_and(|row| !row.archived)
            })
            .map_err(RpcError::BadParams)?;
        for chat_id in &moved {
            self.chats
                .set_chat_archived(chat_id, true)
                .map_err(RpcError::Failed)?;
        }
        Ok(self.roster())
    }

    /// `UnarchiveCaptain`: the marker is spent only after the writes land —
    /// `unarchive_pending` names the crew chats without committing (a
    /// refused write leaves the flag and the marker for a retry to find),
    /// each is woken, and only then does the commit clear the flag and take
    /// the marker. A Captain that is not hidden has nothing pending and
    /// commits nothing.
    fn unarchive(&self, params: &DemoteParams) -> Result<Value, RpcError> {
        let pending = self
            .store
            .unarchive_pending(&params.chat_id)
            .map_err(RpcError::BadParams)?;
        for chat_id in &pending {
            self.chats
                .set_chat_archived(chat_id, false)
                .map_err(RpcError::Failed)?;
        }
        let _ = self
            .store
            .unarchive(&params.chat_id)
            .map_err(RpcError::BadParams)?;
        Ok(self.roster())
    }

    /// `DeleteCaptain`: the record alone drops — the chats it names keep
    /// their rows and the Drive folder is the caller's to keep.
    fn delete(&self, params: &DemoteParams) -> Result<Value, RpcError> {
        self.store
            .delete(&params.chat_id)
            .map_err(RpcError::BadParams)?;
        Ok(self.roster())
    }
}

// ── SUR-210: the per-turn cost scorecard ─────────────────────────────

pub const SCORECARD_FILE: &str = "scorecard.jsonl";

/// One finalized Helm or crew turn, one line of `<drive>/state/scorecard.jsonl`.
/// `cost_usd` is null until a rate card exists — the wire carries tokens, not
/// prices, and an invented price is worse than none.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScorecardEntry {
    #[serde(rename = "chatId")]
    pub chat_id: String,
    pub role: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    pub tokens_in: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_cached: Option<u64>,
    pub tokens_out: u64,
    pub cost_usd: Option<f64>,
    pub duration_s: u64,
    pub outcome: String,
}

/// Captain and crew turns of one roster share the Captain's drive, and their
/// finalizations run on separate blocking threads: the append is serialized
/// so two turns can never interleave a line.
static SCORECARD_LOCK: Mutex<()> = Mutex::new(());

/// Append `entry` to `<drive>/state/scorecard.jsonl` — or don't: one row per
/// `(chatId, turnId)`, so a duplicate finalization of the same turn is a
/// no-op (`Ok(false)`). Existing malformed lines are kept as they are; they
/// are evidence, not the writer's business.
pub fn append_scorecard_entry(drive: &Path, entry: &ScorecardEntry) -> Result<bool, String> {
    let _serialization = SCORECARD_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let state = drive.join("state");
    std::fs::create_dir_all(&state).map_err(|e| format!("scorecard state dir: {e}"))?;
    let path = state.join(SCORECARD_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        for line in existing.lines() {
            if let Ok(row) = serde_json::from_str::<ScorecardEntry>(line)
                && row.chat_id == entry.chat_id
                && row.turn_id == entry.turn_id
            {
                return Ok(false);
            }
        }
    }
    let mut line = serde_json::to_string(entry).map_err(|e| format!("scorecard encode: {e}"))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("scorecard open: {e}"))?;
    // A crash mid-write leaves a tail with no newline. The damaged line is
    // already unparseable; left flush against it, the NEXT row would join it
    // and be unparseable too. Close the line first, then write.
    if std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
        use std::io::{Read, Seek, SeekFrom};
        let mut tail = [0u8; 1];
        let mut read_back = std::fs::File::open(&path).map_err(|e| format!("scorecard read: {e}"))?;
        if read_back.seek(SeekFrom::End(-1)).is_ok()
            && read_back.read_exact(&mut tail).is_ok()
            && tail[0] != b'\n'
        {
            file.write_all(b"\n")
                .map_err(|e| format!("scorecard newline repair: {e}"))?;
        }
    }
    file.write_all(line.as_bytes())
        .map_err(|e| format!("scorecard write: {e}"))?;
    Ok(true)
}

// ── local merge queue (SUR-209) ────────────────────────────────────────────
//
// A Captain marks crew work landable by writing `state/ready/<crewId>.json`
// in its Drive. The engine owns everything under `state/merge-queue/`: an
// atomic lock file, claims, and one result per receipt. The lock makes the
// rebase/gate/land critical section single-writer even when several Captain
// turns finish together.

const MERGE_READY_DIR: &str = "ready";
const MERGE_QUEUE_DIR: &str = "merge-queue";
const MERGE_CLAIMS_DIR: &str = "claims";
const MERGE_RESULTS_DIR: &str = "results";
const MERGE_LOCK_FILE: &str = "captains-merge.lock";

/// A crew branch's ready receipt. `headSha` is the branch tip the Captain
/// reviewed; the queue rebases that exact commit, publishes the rebased tip,
/// and requires GitHub to match that tip when the PR lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadyReceipt {
    pub crew_id: String,
    pub repo: String,
    pub pr: u64,
    pub branch: String,
    pub head_sha: String,
    pub slug: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeOutcomeStatus {
    Landed,
    Aborted,
    Failed,
}

/// The durable answer for one ready receipt, written under
/// `state/merge-queue/results/<crewId>.json` before its claim is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeOutcome {
    pub crew_id: String,
    pub status: MergeOutcomeStatus,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landed_head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MergeProgress {
    original_head: String,
    rebased_head: String,
    target_sha: String,
    #[serde(default)]
    published_heads: Vec<String>,
}

/// A fresh roster check. The closure is evaluated both before the expensive
/// gate and again immediately before landing, so removing a crew member while
/// its merge is queued withdraws that merge.
type LiveCrewCheck<'a> = Option<&'a (dyn Fn(&str) -> bool + Send + Sync)>;

fn crew_is_live(check: LiveCrewCheck<'_>, crew_id: &str) -> bool {
    check.is_none_or(|is_live| is_live(crew_id))
}

fn bare_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !matches!(value, "." | "..")
        && !value.contains('\\')
        && !value.contains('/')
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with('-')
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.ends_with(".lock")
        && !value.contains("..")
        && !value.contains("@{")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.')
        })
}

fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

impl ReadyReceipt {
    fn validate(self) -> Result<Self, String> {
        if !bare_file_name(&self.crew_id) {
            return Err("crewId must be a bare file name".into());
        }
        if self.pr == 0 {
            return Err("pr must be a pull request number".into());
        }
        if !Path::new(&self.repo).is_absolute() {
            return Err("repo must be an absolute path".into());
        }
        if !valid_branch(&self.branch) {
            return Err("branch is not a safe git branch name".into());
        }
        if !valid_sha(&self.head_sha) {
            return Err("headSha must be a full commit SHA".into());
        }
        if !valid_slug(&self.slug) {
            return Err("slug must be lowercase letters, digits and dashes".into());
        }
        Ok(self)
    }
}

/// The operations the queue cannot perform in-process. Tests substitute a
/// runner so concurrency and target movement are deterministic.
pub(crate) trait MergeRunner: Send + Sync {
    fn fetch(&self, repo: &Path, branch: &str) -> Result<(), String>;
    fn target_sha(&self, repo: &Path) -> Result<String, String>;
    fn branch_sha(&self, repo: &Path, branch: &str) -> Result<String, String>;
    fn pull_request_base(&self, repo: &Path, pr: u64) -> Result<String, String>;
    fn pull_request_state(&self, repo: &Path, pr: u64) -> Result<String, String>;
    fn pull_request_head(&self, repo: &Path, pr: u64) -> Result<String, String>;
    fn changed_files(&self, repo: &Path, base: &str, head: &str) -> Result<Vec<String>, String>;
    fn file_exists_at(&self, repo: &Path, head: &str, path: &str) -> Result<bool, String>;
    /// Rebase `head` onto `origin/main` in `scratch`, run the local gates,
    /// publish the rebased branch tip without overwriting a newer remote tip,
    /// and return that tip.
    fn rebase_gate_publish(
        &self,
        repo: &Path,
        branch: &str,
        head: &str,
        lease: &str,
        scratch: &Path,
        before_publish: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<String, String>;
    fn land(&self, repo: &Path, pr: u64, head: &str) -> Result<String, String>;
}

struct ShellMergeRunner;

impl ShellMergeRunner {
    fn command(cwd: &Path, program: &str, args: &[String]) -> Result<String, String> {
        let output = std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|error| format!("could not start {program}: {error}"))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            if stderr.is_empty() { format!("exit {:?}", output.status.code()) } else { stderr }
        ))
    }

    fn git(repo: &Path, args: &[String]) -> Result<String, String> {
        let mut full = vec!["-C".to_string(), repo.to_string_lossy().into_owned()];
        full.extend(args.iter().cloned());
        Self::command(repo, "git", &full)
    }
}

impl MergeRunner for ShellMergeRunner {
    fn fetch(&self, repo: &Path, branch: &str) -> Result<(), String> {
        Self::git(
            repo,
            &[
                "fetch".into(),
                "origin".into(),
                "+refs/heads/main:refs/remotes/origin/main".into(),
                format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
            ],
        )
        .map(|_| ())
    }

    fn target_sha(&self, repo: &Path) -> Result<String, String> {
        Self::git(repo, &["rev-parse".into(), "origin/main".into()])
    }

    fn branch_sha(&self, repo: &Path, branch: &str) -> Result<String, String> {
        Self::git(
            repo,
            &[
                "rev-parse".into(),
                format!("refs/remotes/origin/{branch}"),
            ],
        )
    }

    fn pull_request_base(&self, repo: &Path, pr: u64) -> Result<String, String> {
        Self::command(
            repo,
            "gh",
            &[
                "pr".into(),
                "view".into(),
                pr.to_string(),
                "--json".into(),
                "baseRefName".into(),
                "--jq".into(),
                ".baseRefName".into(),
            ],
        )
    }

    fn pull_request_state(&self, repo: &Path, pr: u64) -> Result<String, String> {
        Self::command(
            repo,
            "gh",
            &[
                "pr".into(),
                "view".into(),
                pr.to_string(),
                "--json".into(),
                "state".into(),
                "--jq".into(),
                ".state".into(),
            ],
        )
    }

    fn pull_request_head(&self, repo: &Path, pr: u64) -> Result<String, String> {
        Self::command(
            repo,
            "gh",
            &[
                "pr".into(),
                "view".into(),
                pr.to_string(),
                "--json".into(),
                "headRefOid".into(),
                "--jq".into(),
                ".headRefOid".into(),
            ],
        )
    }

    fn changed_files(&self, repo: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
        let range = format!("{base}...{head}");
        Self::git(repo, &["diff".into(), "--name-only".into(), range])
            .map(|output| output.lines().map(str::to_owned).collect())
    }

    fn file_exists_at(&self, repo: &Path, head: &str, path: &str) -> Result<bool, String> {
        Self::git(
            repo,
            &[
                "ls-tree".into(),
                head.into(),
                "--".into(),
                path.into(),
            ],
        )
        .map(|output| {
            output.lines().any(|line| {
                let mut parts = line.split_whitespace();
                parts.next().is_some()
                    && parts.next() == Some("blob")
                    && parts.next().is_some()
                    && line.rsplit_once('\t').is_some_and(|(_, name)| name == path)
            })
        })
    }

    fn rebase_gate_publish(
        &self,
        repo: &Path,
        branch: &str,
        head: &str,
        lease: &str,
        scratch: &Path,
        before_publish: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        let worktree = scratch.join("checkout");
        if worktree.exists() {
            std::fs::remove_dir_all(&worktree)
                .map_err(|error| format!("could not clear the merge worktree: {error}"))?;
        }
        // A crash can leave the checkout missing while `.git/worktrees` still
        // registers it. Prune before recreating the path; waiting until after
        // the gate makes crash recovery fail at `worktree add` forever.
        Self::git(repo, &["worktree".into(), "prune".into()])?;
        let worktree_text = worktree.to_string_lossy().into_owned();
        Self::git(
            repo,
            &[
                "worktree".into(),
                "add".into(),
                "--detach".into(),
                worktree_text.clone(),
                head.into(),
            ],
        )?;
        let result = (|| {
            Self::command(&worktree, "git", &["rebase".into(), "origin/main".into()])?;
            Self::command(
                &worktree,
                "cargo",
                &["nextest".into(), "run".into(), "--locked".into()],
            )?;
            Self::command(&worktree.join("apps/zgx"), "zgx", &["test".into(), "ui".into()])?;
            let new_head = Self::command(&worktree, "git", &["rev-parse".into(), "HEAD".into()])?;
            before_publish(&new_head)?;
            Self::command(
                &worktree,
                "git",
                &[
                    "push".into(),
                    format!("--force-with-lease=refs/heads/{branch}:{lease}"),
                    "origin".into(),
                    format!("HEAD:refs/heads/{branch}"),
                ],
            )?;
            Ok(new_head)
        })();
        let _ = Self::git(
            repo,
            &[
                "worktree".into(),
                "remove".into(),
                "--force".into(),
                worktree_text,
            ],
        );
        let _ = Self::git(repo, &["worktree".into(), "prune".into()]);
        result
    }

    fn land(&self, repo: &Path, pr: u64, head: &str) -> Result<String, String> {
        let output = Self::command(
            repo,
            "gh",
            &[
                "pr".into(),
                "merge".into(),
                pr.to_string(),
                "--squash".into(),
                "--match-head-commit".into(),
                head.into(),
            ],
        )?;
        let state = Self::command(
            repo,
            "gh",
            &[
                "pr".into(),
                "view".into(),
                pr.to_string(),
                "--json".into(),
                "state".into(),
                "--jq".into(),
                ".state".into(),
            ],
        )?;
        if state == "MERGED" {
            Ok(output)
        } else {
            Err(format!("gh pr merge did not land the pull request; its state is {state}"))
        }
    }
}

struct MergeMutexGuard {
    path: PathBuf,
}

impl MergeMutexGuard {
    fn owner_alive(path: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else {
            return true;
        };
        let Some(pid) = text
            .lines()
            .find_map(|line| line.strip_prefix("pid="))
            .and_then(|pid| pid.split_whitespace().next()?.parse::<u32>().ok())
        else {
            // A creator can be between create_new and writing its metadata.
            // Treat only a malformed file that has had time to be written as stale.
            let age = path
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok());
            return age.is_none_or(|age| age < std::time::Duration::from_secs(2));
        };
        let pid = sysinfo::Pid::from_u32(pid);
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid])) > 0
    }

    fn acquire(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|error| format!("could not create the merge queue: {error}"))?;
        let path = dir.join(MERGE_LOCK_FILE);
        let mut checked = std::time::Instant::now() - std::time::Duration::from_secs(2);
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "pid={}", std::process::id());
                    let _ = writeln!(file, "created={}", Utc::now().to_rfc3339());
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if checked.elapsed() >= std::time::Duration::from_secs(1) {
                        checked = std::time::Instant::now();
                        // create_new survives an engine crash. The owner pid
                        // inside the file is what makes the lock self-healing:
                        // a dead owner cannot keep every later merge queued.
                        if !Self::owner_alive(&path)
                            && matches!(std::fs::remove_file(&path), Ok(()))
                        {
                            continue;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => return Err(format!("could not take the merge mutex: {error}")),
            }
        }
    }
}

impl Drop for MergeMutexGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn merge_fragment_failure(files: &[String], slug: &str, fragment_exists: bool) -> Option<String> {
    if let Some(path) = files
        .iter()
        .find(|path| path.as_str() == "VERSION" || path.as_str() == "CHANGELOG.md")
    {
        return Some(format!(
            "crew branches must not stamp {path}; add a changelog.d fragment instead"
        ));
    }
    let fragment = format!("changelog.d/{slug}.md");
    if !files.iter().any(|path| path == &fragment) || !fragment_exists {
        return Some(format!("crew branch is missing {fragment}"));
    }
    None
}

fn write_merge_progress(
    path: &Path,
    original_head: &str,
    rebased_head: &str,
    target_sha: &str,
    published_heads: &[String],
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create the merge progress directory: {error}"))?;
    }
    let progress = MergeProgress {
        original_head: original_head.into(),
        rebased_head: rebased_head.into(),
        target_sha: target_sha.into(),
        published_heads: published_heads.to_vec(),
    };
    let json = serde_json::to_string(&progress)
        .map_err(|error| format!("could not encode merge progress: {error}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|error| format!("could not write merge progress: {error}"))
}

fn read_merge_progress(path: &Path) -> Option<MergeProgress> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn perform_merge(
    receipt: &ReadyReceipt,
    scratch: &Path,
    runner: &dyn MergeRunner,
    live_crew: LiveCrewCheck<'_>,
) -> MergeOutcome {
    let failure = |reason: String| MergeOutcome {
        crew_id: receipt.crew_id.clone(),
        status: MergeOutcomeStatus::Failed,
        reason,
        landed_head: None,
    };
    let repo = Path::new(&receipt.repo);
    let result = (|| {
        let base = runner.pull_request_base(repo, receipt.pr)?;
        if base != "main" {
            return Err(format!("crew merge queue only lands pull requests targeting main, not {base}"));
        }
        let progress_path = scratch.join("progress.json");
        let progress = read_merge_progress(&progress_path)
            .filter(|progress| progress.original_head == receipt.head_sha);
        match runner.pull_request_state(repo, receipt.pr)?.as_str() {
            // A crash can happen after `gh pr merge` has already landed the
            // PR. Count that as recovery only when GitHub's final head is the
            // reviewed commit or a rebased head this queue recorded; otherwise
            // a maintainer merged a different revision and the Captain must
            // reconcile that divergence instead of receiving a false success.
            "MERGED" => {
                let merged_head = runner.pull_request_head(repo, receipt.pr)?;
                let reviewed = merged_head == receipt.head_sha
                    || progress.as_ref().is_some_and(|progress| {
                        merged_head == progress.rebased_head
                            || progress.published_heads.iter().any(|head| head == &merged_head)
                    });
                if reviewed {
                    return Ok(MergeOutcome {
                        crew_id: receipt.crew_id.clone(),
                        status: MergeOutcomeStatus::Landed,
                        reason: "pull request was already merged".into(),
                        landed_head: Some(merged_head),
                    });
                }
                return Err(format!(
                    "pull request was already merged from unreviewed head {merged_head}"
                ));
            }
            "CLOSED" => return Err("pull request is closed without merging".into()),
            _ => {}
        }
        runner.fetch(repo, &receipt.branch)?;
        let current_target = runner.target_sha(repo)?;
        let remote_head = runner.branch_sha(repo, &receipt.branch)?;
        let mut published = None;
        let mut lease = receipt.head_sha.clone();
        let mut before = current_target;
        let mut published_heads = progress
            .as_ref()
            .map(|progress| progress.published_heads.clone())
            .unwrap_or_default();
        let mut remote_is_self_published = false;
        if let Some(progress) = &progress {
            if remote_head == progress.rebased_head {
                remote_is_self_published = true;
                if progress.target_sha == before {
                    published = Some(progress.rebased_head.clone());
                    before = progress.target_sha.clone();
                } else {
                    // The published head is our own, but it was tested against
                    // a main that has since moved. Re-rebase the reviewed head
                    // and lease only that known self-published tip.
                    lease = remote_head.clone();
                }
            } else if progress.published_heads.iter().any(|head| head == &remote_head) {
                // A replacement push can fail after progress advanced. The
                // branch then still points at an older queue-published head;
                // that tip is safe to lease even though it is no longer the
                // latest candidate recorded in progress.
                remote_is_self_published = true;
                lease = remote_head.clone();
            } else if remote_head != receipt.head_sha {
                return Err(format!(
                    "crew branch tip {remote_head} matches neither the reviewed head {} nor a queue-published head",
                    receipt.head_sha
                ));
            }
        } else if remote_head != receipt.head_sha {
            return Err(format!(
                "crew branch tip {remote_head} no longer matches the reviewed head {}",
                receipt.head_sha
            ));
        }
        if remote_is_self_published && !published_heads.iter().any(|head| head == &remote_head) {
            published_heads.push(remote_head.clone());
        }
        let files = runner.changed_files(repo, &before, &receipt.head_sha)?;
        let fragment = format!("changelog.d/{}.md", receipt.slug);
        let fragment_exists = runner.file_exists_at(repo, &receipt.head_sha, &fragment)?;
        if let Some(reason) = merge_fragment_failure(&files, &receipt.slug, fragment_exists) {
            return Err(reason);
        }
        let head = if let Some(head) = published {
            head
        } else {
            let before_publish = |rebased_head: &str| {
                write_merge_progress(
                    &progress_path,
                    &receipt.head_sha,
                    rebased_head,
                    &before,
                    &published_heads,
                )
            };
            runner.rebase_gate_publish(
                repo,
                &receipt.branch,
                &receipt.head_sha,
                &lease,
                scratch,
                &before_publish,
            )?
        };
        runner.fetch(repo, &receipt.branch)?;
        let after = runner.target_sha(repo)?;
        if after != before {
            return Ok(MergeOutcome {
                crew_id: receipt.crew_id.clone(),
                status: MergeOutcomeStatus::Aborted,
                reason: format!("target head moved while the merge gate ran: {before} -> {after}"),
                landed_head: None,
            });
        }
        if !crew_is_live(live_crew, &receipt.crew_id) {
            return Err("crew member was removed while the merge gate ran".into());
        }
        runner.land(repo, receipt.pr, &head)?;
        Ok(MergeOutcome {
            crew_id: receipt.crew_id.clone(),
            status: MergeOutcomeStatus::Landed,
            reason: "landed".into(),
            landed_head: Some(head),
        })
    })();
    result.unwrap_or_else(failure)
}

fn write_merge_outcome(path: &Path, outcome: &MergeOutcome) {
    match serde_json::to_string_pretty(outcome) {
        Ok(json) => {
            let tmp = path.with_extension("json.tmp");
            let written = std::fs::write(&tmp, json)
                .and_then(|()| std::fs::rename(&tmp, path));
            if let Err(error) = written {
                let _ = std::fs::remove_file(&tmp);
                tracing::warn!(path = %path.display(), error = %error, "merge outcome write failed");
            }
        }
        Err(error) => tracing::warn!(error = %error, "merge outcome serialization failed"),
    }
}

fn json_file_names(dir: &Path) -> Result<Vec<String>, std::io::Error> {
    let mut names = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.ends_with(".json"))
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

fn read_json_file_names(dir: &Path) -> Option<Vec<String>> {
    match json_file_names(dir) {
        Ok(names) => Some(names),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(error) => {
            tracing::warn!(path = %dir.display(), error = %error, "merge queue scan failed");
            None
        }
    }
}

fn merge_lock_root(drive: &Path, queue: &Path) -> PathBuf {
    // In production every Drive is `<deepseek-home>/captains/<id>`; locking
    // the captains root makes the merge writer global across Captains on this
    // device. A nonstandard Drive (tests and repair tools) falls back to its
    // own queue rather than inventing a lock in an unrelated parent such as
    // `/tmp`.
    drive
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "captains"))
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| queue.to_path_buf())
}

fn process_ready_claim(
    drive: &Path,
    crew_id: &str,
    claim: &Path,
    runner: &dyn MergeRunner,
    live_crew: LiveCrewCheck<'_>,
) -> MergeOutcome {
    let queue = drive.join("state").join(MERGE_QUEUE_DIR);
    let outcome = match std::fs::read_to_string(claim)
        .map_err(|error| format!("could not read the ready receipt: {error}"))
        .and_then(|text| {
            serde_json::from_str::<ReadyReceipt>(&text)
                .map_err(|error| format!("invalid ready receipt: {error}"))
        })
        .and_then(ReadyReceipt::validate)
    {
        Ok(receipt) if receipt.crew_id != crew_id => MergeOutcome {
            crew_id: crew_id.to_string(),
            status: MergeOutcomeStatus::Failed,
            reason: "ready receipt crewId does not match its file name".into(),
            landed_head: None,
        },
        Ok(receipt) if !crew_is_live(live_crew, &receipt.crew_id) => MergeOutcome {
            crew_id: crew_id.to_string(),
            status: MergeOutcomeStatus::Failed,
            reason: "ready receipt does not name a live member of this Captain's crew".into(),
            landed_head: None,
        },
        Ok(receipt) => perform_merge(
            &receipt,
            &queue.join(&receipt.crew_id),
            runner,
            live_crew,
        ),
        Err(reason) => MergeOutcome {
            crew_id: crew_id.to_string(),
            status: MergeOutcomeStatus::Failed,
            reason,
            landed_head: None,
        },
    };
    write_merge_outcome(
        &queue.join(MERGE_RESULTS_DIR).join(format!("{crew_id}.json")),
        &outcome,
    );
    let _ = std::fs::remove_file(claim);
    outcome
}

/// Process every ready receipt currently in this Captain Drive. The scan holds
/// the merge mutex before claiming or recovering receipts. Consequently, once
/// the mutex is acquired any file already in `claims/` is stranded by a dead
/// scanner rather than actively processed: it is safe to settle it again.
/// Receipts are still claimed by atomic rename so a Captain racing the scan
/// cannot observe a half-written input as work in progress. This is
/// deliberately synchronous: callers run it in a blocking task.
pub(crate) fn process_ready_receipts_with(
    drive: &Path,
    runner: &dyn MergeRunner,
    live_crew: LiveCrewCheck<'_>,
) -> Vec<MergeOutcome> {
    let state = drive.join("state");
    let ready = state.join(MERGE_READY_DIR);
    let queue = state.join(MERGE_QUEUE_DIR);
    let claims = queue.join(MERGE_CLAIMS_DIR);
    for dir in [&claims, &queue.join(MERGE_RESULTS_DIR)] {
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!(path = %dir.display(), error = %error, "merge queue state is unavailable");
            return Vec::new();
        }
    }
    let Some(initial_ready) = read_json_file_names(&ready) else { return Vec::new() };
    let Some(initial_claims) = read_json_file_names(&claims) else { return Vec::new() };
    if initial_ready.is_empty() && initial_claims.is_empty() {
        return Vec::new();
    }
    let lock_root = merge_lock_root(drive, &queue);
    let _lock = match MergeMutexGuard::acquire(&lock_root) {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(path = %lock_root.display(), error = %error, "merge mutex is unavailable");
            return Vec::new();
        }
    };

    let mut outcomes = Vec::new();
    // Recover stranded claims first. A fresh ready receipt for the same crew
    // is therefore settled after the older attempt and writes the final result.
    let Some(claim_names) = read_json_file_names(&claims) else { return outcomes };
    for name in claim_names {
        let crew_id = name.trim_end_matches(".json");
        if !bare_file_name(crew_id) || crew_id.contains(".") {
            continue;
        }
        outcomes.push(process_ready_claim(
            drive,
            crew_id,
            &claims.join(&name),
            runner,
            live_crew,
        ));
    }

    let Some(names) = read_json_file_names(&ready) else { return outcomes };
    for name in names {
        let crew_id = name.trim_end_matches(".json");
        if !bare_file_name(crew_id) || crew_id.contains(".") {
            continue;
        }
        let from = ready.join(&name);
        let claim = claims.join(&name);
        // rename is atomic and fails for every scanner after the first.
        if std::fs::rename(&from, &claim).is_err() {
            continue;
        }
        outcomes.push(process_ready_claim(drive, crew_id, &claim, runner, live_crew));
    }
    outcomes
}

/// Process ready receipts with the local `git`, `cargo`, `zgx` and `gh`
/// commands. The roster check reloads the registry so a crew removal during a
/// long gate is observed before landing. Errors become per-crew result files;
/// scanning itself never throws.
pub fn process_ready_receipts(drive: &Path, data_dir: &Path, captain_id: &str) -> Vec<MergeOutcome> {
    if let Err(error) = Registry::load(data_dir) {
        tracing::warn!(error = %error, "crew merge queue skipped: captains registry load failed");
        return Vec::new();
    }
    let live_crew = |crew_id: &str| {
        Registry::load(data_dir).ok().is_some_and(|registry| {
            registry
                .captain_of_crew(crew_id)
                .is_some_and(|captain| captain.chat_id == captain_id && captain.live())
        })
    };
    process_ready_receipts_with(drive, &ShellMergeRunner, Some(&live_crew))
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
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::Arc;

    /// SUR-214: [`handles`] routes the roster surface from one table, and the
    /// relay must refuse every method in it before a peer's call reaches this
    /// engine. A method added to the table without a `local_only` entry would be
    /// relay-reachable until a review noticed, so this pins the two together.
    #[test]
    fn every_captain_roster_method_is_local_only() {
        assert!(!SURFACE.is_empty(), "the roster surface is empty");
        for &method in SURFACE {
            assert!(handles(method), "{method} is in SURFACE but not routed");
            assert!(
                zeron_rpc::local_only(method),
                "{method} is routed to the Captain roster but is not `zeron_rpc::local_only`: \
                 a peer device's relay would carry it here"
            );
            assert!(
                zeron_rpc::peer_refused(method, &serde_json::json!({ "chatId": "chat-1" })),
                "{method} is not refused for a peer device"
            );
            assert!(
                !zeron_rpc::agent_allowed(method),
                "{method} is agent-allowed; the roster belongs to the person's own window"
            );
        }
        // `handles` is the table and nothing else: a method outside it is never
        // routed here, so the pin above covers the whole surface.
        for method in [methods::AUTH_STATUS, methods::ENGINE_INFO] {
            assert!(!handles(method), "{method} is routed to the Captain roster");
        }
    }

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
        let store = CaptainsStore::open(dir.path().to_path_buf());
        assert!(store.snapshot().captains().is_empty());

        store.promote("chat-1", "ada").unwrap();
        let reopened = Registry::load(dir.path()).unwrap();
        assert!(reopened.is_captain("chat-1"));
        assert_eq!(store.snapshot(), reopened);
    }

    /// SUR-215: a `captains.json` that would not load at `open` degrades to an
    /// empty roster, and the degrade must not latch reads either — an external
    /// repair reaches `snapshot()` with no mutation in between and no reopen.
    #[test]
    fn a_repair_to_an_unreadable_file_is_visible_to_the_same_store() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("captains.json"), b"{ not json").unwrap();
        let store = CaptainsStore::open(dir.path().to_path_buf());
        assert!(
            store.snapshot().captains().is_empty(),
            "an unreadable file degrades to an empty roster"
        );

        // Repair the file the way another writer would: a complete, valid
        // document, saved atomically over the broken one.
        let mut repaired = Registry::default();
        repaired.promote("cap-a", "ada").unwrap();
        repaired.save(dir.path()).unwrap();

        let roster = store.snapshot();
        assert_eq!(
            roster.captains().len(),
            1,
            "the repaired Captain is visible to the same store: {roster:?}"
        );
        assert!(roster.is_captain("cap-a"));
        assert_eq!(
            roster.run_role("cap-a"),
            RunRole::Captain { name: "ada".into() }
        );
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
                captain_name: "ada".into(),
                owns: Vec::new(),
                worktree: None,
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
            drive_dir(home, "chat-1").to_string_lossy().as_ref()
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
            drive_dir(home, "chat-1").to_string_lossy().as_ref()
        );
    }

    #[test]
    fn helm_owns_metadata_round_trips_into_the_crew_stamp() {
        let home = Path::new("/deepseek-home");
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        let worktree = if cfg!(windows) {
            "C:/worktrees/chat-2"
        } else {
            "/worktrees/chat-2"
        };
        registry
            .add_crew_with_metadata(
                "chat-1",
                "chat-2",
                CrewMeta {
                    owns: vec!["src/**".into(), "docs/*.md".into()],
                    worktree: Some(worktree.into()),
                },
            )
            .unwrap();

        let stamp: serde_json::Value =
            serde_json::from_str(&captains_env(&registry, home, "chat-2").unwrap()).unwrap();
        assert_eq!(stamp["owns"], serde_json::json!(["src/**", "docs/*.md"]));
        assert_eq!(stamp["worktree"], worktree);

        let encoded = serde_json::to_string_pretty(&registry).unwrap();
        let loaded: Registry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(loaded, registry);
    }

    #[test]
    fn helm_owns_defaults_keep_legacy_stamps_unchanged() {
        let home = Path::new("/deepseek-home");
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        registry.add_crew("chat-1", "chat-2").unwrap();
        let stamp: serde_json::Value =
            serde_json::from_str(&captains_env(&registry, home, "chat-2").unwrap()).unwrap();
        assert!(stamp.get("owns").is_none());
        assert!(stamp.get("worktree").is_none());
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
        let first = CaptainsStore::open(dir.path().to_path_buf());
        let second = CaptainsStore::open(dir.path().to_path_buf());
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

        let store = CaptainsStore::open(dir.path().to_path_buf());
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
        let store = CaptainsStore::open(dir.path().to_path_buf());
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

    /// The host's chat rows the rule checks read, as a plain map — plus the
    /// cascade's own writes, recorded in order so a test can assert the exact
    /// ids a Captain archive moved (SUR-94: "the crew goes to sleep" is the
    /// ticket's headline, so an unobserved no-op write would pass a test that
    /// only looked at the registry). `refuse` makes the next N writes fail:
    /// the partial-failure seam a retry has to survive.
    #[derive(Default)]
    struct FakeChats {
        rows: HashMap<String, ChatRow>,
        writes: Rc<RefCell<Vec<(String, bool)>>>,
        refuse: Rc<Cell<usize>>,
    }

    impl FakeChats {
        fn row(mut self, chat_id: &str, space: Option<&str>, parent: Option<&str>) -> Self {
            self.rows.insert(
                chat_id.to_string(),
                ChatRow {
                    space_id: space.map(str::to_string),
                    parent_chat_id: parent.map(str::to_string),
                    device_id: DEVICE.to_string(),
                    turn_in_flight: false,
                    archived: false,
                },
            );
            self
        }

        fn other_device(mut self, chat_id: &str) -> Self {
            if let Some(row) = self.rows.get_mut(chat_id) {
                row.device_id = "device-2".to_string();
            }
            self
        }

        fn busy(mut self, chat_id: &str) -> Self {
            if let Some(row) = self.rows.get_mut(chat_id) {
                row.turn_in_flight = true;
            }
            self
        }

        fn archived(mut self, chat_id: &str) -> Self {
            if let Some(row) = self.rows.get_mut(chat_id) {
                row.archived = true;
            }
            self
        }
    }

    impl Chats for FakeChats {
        fn chat(&self, chat_id: &str) -> Option<ChatRow> {
            self.rows.get(chat_id).cloned()
        }

        fn set_chat_archived(&self, chat_id: &str, archived: bool) -> Result<(), String> {
            self.writes
                .borrow_mut()
                .push((chat_id.to_string(), archived));
            let left = self.refuse.get();
            if left > 0 {
                self.refuse.set(left - 1);
                return Err(format!("the workspace refused to write {chat_id}"));
            }
            Ok(())
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
            let store = CaptainsStore::open(dir.path().to_path_buf());
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

    /// SUR-190: the project rule gates `AddCrew`, not `RemoveCrew`. A member
    /// whose chat row is re-assigned to another project after the add must
    /// still be releasable — otherwise the stale crew entry has no API that
    /// can clear it.
    #[test]
    fn a_crew_member_whose_chat_moved_to_another_project_can_still_be_removed() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-x", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-x"}),
        )
        .unwrap();
        assert_eq!(crew(&rig.roster(), "cap-a"), vec!["chat-x".to_string()]);
        let commits = rig.store.commits();

        // The chat row is re-assigned to another project after the add.
        let moved = FakeChats::default()
            .row("cap-a", Some("space-1"), None)
            .row("chat-x", Some("space-2"), None);
        let rpc = CaptainsRpc {
            store: &rig.store,
            chats: &moved,
            device_id: DEVICE,
            deepseek_home: &rig.home,
        };
        rpc.call(
            methods::REMOVE_CREW,
            &json!({"captainId": "cap-a", "chatId": "chat-x"}),
        )
        .expect("a member whose row moved projects must still be removable");
        assert!(
            crew(&rig.roster(), "cap-a").is_empty(),
            "the roster drops the moved member"
        );
        assert_eq!(
            rig.store.commits(),
            commits + 1,
            "the removal is one committed change"
        );
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
                archived: false,
            })
        }

        fn set_chat_archived(&self, _chat_id: &str, _archived: bool) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn add_crew_rechecks_the_project_under_the_store_lock() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let store = CaptainsStore::open(dir.path().to_path_buf());
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

    /// The row that answers the operation's reads and is gone immediately
    /// after: a delete landing between the under-lock re-read and the save,
    /// the window the chat store's own lock cannot close because it takes no
    /// part in `MUTATION`. The member answers the two reads a successful add
    /// makes (its pre-check and the under-lock pair), then reports gone.
    struct VanishingChat {
        member: String,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl Chats for VanishingChat {
        fn chat(&self, chat_id: &str) -> Option<ChatRow> {
            if chat_id == self.member {
                let seen = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if seen >= 2 {
                    return None;
                }
            }
            Some(ChatRow {
                space_id: Some("space-1".to_string()),
                parent_chat_id: None,
                device_id: DEVICE.to_string(),
                turn_in_flight: false,
                archived: false,
            })
        }

        fn set_chat_archived(&self, _chat_id: &str, _archived: bool) -> Result<(), String> {
            Ok(())
        }
    }

    /// SUR-189: a delete that lands after the under-lock row re-read leaves a
    /// committed record for a chat that no longer exists — the `committed_row`
    /// doc's residual, and the roster keeps it until it is repaired. The
    /// repair path is `RemoveCrew`: a member row that is gone is the dangling
    /// record this call exists to clear, not a bad argument.
    #[test]
    fn a_crew_entry_left_by_a_delete_in_the_commit_window_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let store = CaptainsStore::open(dir.path().to_path_buf());
        store.promote("cap-a", "Ada").unwrap();
        let chats = VanishingChat {
            member: "chat-x".to_string(),
            reads: std::sync::atomic::AtomicUsize::new(0),
        };
        let rpc = CaptainsRpc {
            store: &store,
            chats: &chats,
            device_id: DEVICE,
            deepseek_home: &home,
        };

        rpc.call(
            methods::ADD_CREW,
            &json!({"captainId": "cap-a", "chatId": "chat-x"}),
        )
        .expect("the add sees the row: the delete lands after its under-lock re-read");
        let roster = rpc.call(methods::CAPTAINS, &json!({})).unwrap();
        assert_eq!(
            crew(&roster, "cap-a"),
            vec!["chat-x".to_string()],
            "the committed record survives the delete: the documented residual"
        );

        rpc.call(
            methods::REMOVE_CREW,
            &json!({"captainId": "cap-a", "chatId": "chat-x"}),
        )
        .expect("a dangling crew entry is repairable without its row");
        let roster = rpc.call(methods::CAPTAINS, &json!({})).unwrap();
        assert!(
            crew(&roster, "cap-a").is_empty(),
            "the roster drops the repaired entry: {:?}",
            crew(&roster, "cap-a")
        );
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

    /// The plugin reads the row's `driveRoot` as the Drive itself
    /// (`plugins/dsh-captains/src/tools.js`: `driveDir(cfg) = cfg.driveRoot`),
    /// so the stamp must name the directory `ensure_drive` creates, never its
    /// parent.
    #[test]
    fn the_stamp_names_the_drive_ensure_drive_creates() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let mut registry = Registry::default();
        registry.promote("chat-1", "ada").unwrap();
        registry.add_crew("chat-1", "chat-2").unwrap();

        for chat_id in ["chat-1", "chat-2"] {
            let stamp: serde_json::Value =
                serde_json::from_str(&captains_env(&registry, &home, chat_id).unwrap()).unwrap();
            let drive = ensure_drive(&registry, &home, chat_id).unwrap().unwrap();
            assert_eq!(
                stamp["driveRoot"].as_str().unwrap(),
                drive.to_string_lossy().as_ref(),
                "{chat_id}: the stamp names the Drive the engine creates"
            );
            assert_ne!(
                drive,
                captains_root(&home),
                "{chat_id}: the Drive is not the captains root"
            );
            assert!(drive.is_dir(), "{chat_id}: the Drive exists after ensure_drive");
        }
    }

    #[test]
    fn ensure_drive_refuses_an_id_the_plugin_would_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        for id in ["../../escape", "a/b", "", "with space", "dotted."] {
            let mut registry = Registry::default();
            registry.promote(id, "ada").unwrap();
            assert_eq!(
                ensure_drive(&registry, &home, id).unwrap(),
                None,
                "id {id:?} was not skipped"
            );
        }
        assert!(
            !captains_root(&home).exists(),
            "an invalid id created no drive"
        );
    }

    #[test]
    fn two_simultaneous_first_turns_get_the_exact_layout() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();
        let registry = Arc::new(registry);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let registry = Arc::clone(&registry);
            let home = home.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                ensure_drive(&registry, &home, "cap-a")
            }));
        }
        for handle in handles {
            handle.join().unwrap().unwrap().unwrap();
        }

        let drive = drive_dir(&home, "cap-a");
        let mut names: Vec<String> = std::fs::read_dir(&drive)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "AGENTS.md", "CREW.md", "DIGEST.md", "NOTES.md", "handoffs", "renders",
                "reports", "skills", "state",
            ],
            "the exact nine-entry layout, once"
        );
        for file in ["AGENTS.md", "CREW.md", "NOTES.md", "DIGEST.md"] {
            let text = std::fs::read_to_string(drive.join(file)).unwrap();
            assert!(!text.trim().is_empty(), "{file} is empty");
        }
        for sub in ["skills", "reports", "renders", "handoffs", "state"] {
            assert!(drive.join(sub).is_dir(), "{sub}/ is missing");
        }
    }

    #[test]
    fn a_half_initialized_drive_keeps_its_notes() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let mut registry = Registry::default();
        registry.promote("cap-a", "ada").unwrap();

        let drive = drive_dir(&home, "cap-a");
        std::fs::create_dir_all(&drive).unwrap();
        let edited = "# my plan\n\nkeep me\n";
        std::fs::write(drive.join("NOTES.md"), edited).unwrap();

        ensure_drive(&registry, &home, "cap-a").unwrap().unwrap();

        assert_eq!(
            std::fs::read_to_string(drive.join("NOTES.md")).unwrap(),
            edited,
            "a retry must not overwrite an edited NOTES.md"
        );
        for file in ["AGENTS.md", "CREW.md", "DIGEST.md"] {
            assert!(drive.join(file).is_file(), "{file} was not completed");
        }
        for sub in ["skills", "reports", "renders", "handoffs", "state"] {
            assert!(drive.join(sub).is_dir(), "{sub}/ was not completed");
        }
    }

    /// A row the plugin would refuse at `normalizeConfig` must not be stamped:
    /// the plugin throws before a single registration runs, so the run would
    /// die on the very stamp the row exists to enable. The same predicate is
    /// consulted by `ensure_drive`, so both call sites agree.
    fn invalid_row_registry() -> Registry {
        let at = "2026-11-01T00:00:00Z".parse().unwrap();
        Registry {
            captains: vec![
                Captain {
                    chat_id: "bad/id".to_string(),
                    name: "Ada".to_string(),
                    created_at: at,
                    archived: false,
                    user_archived: false,
                    crew: vec!["crew-x".to_string()],
                    archived_crew: Vec::new(),
                    crew_meta: BTreeMap::new(),
                },
                Captain {
                    chat_id: "cap-name".to_string(),
                    name: "x".repeat(81),
                    created_at: at,
                    archived: false,
                    user_archived: false,
                    crew: Vec::new(),
                    archived_crew: Vec::new(),
                    crew_meta: BTreeMap::new(),
                },
                Captain {
                    chat_id: "cap-ok".to_string(),
                    name: "Grace".to_string(),
                    created_at: at,
                    archived: false,
                    user_archived: false,
                    crew: Vec::new(),
                    archived_crew: Vec::new(),
                    crew_meta: BTreeMap::new(),
                },
            ],
        }
    }

    #[test]
    fn captains_env_skips_a_row_the_plugin_would_refuse() {
        let home = Path::new("/deepseek-home");
        let registry = invalid_row_registry();
        assert!(
            captains_env(&registry, home, "bad/id").is_none(),
            "an invalid Captain id was stamped"
        );
        assert!(
            captains_env(&registry, home, "crew-x").is_none(),
            "a crew of an invalid Captain id was stamped"
        );
        assert!(
            captains_env(&registry, home, "cap-name").is_none(),
            "a Captain name the plugin refuses was stamped"
        );
        assert!(
            captains_env(&registry, home, "cap-ok").is_some(),
            "a valid row is still stamped"
        );
    }

    #[test]
    fn ensure_drive_skips_a_row_the_plugin_would_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let registry = invalid_row_registry();
        for chat_id in ["bad/id", "crew-x", "cap-name"] {
            assert_eq!(
                ensure_drive(&registry, &home, chat_id).unwrap(),
                None,
                "{chat_id}: a refused row creates no drive and does not error"
            );
        }
        assert!(
            !captains_root(&home).exists(),
            "a refused row created the captains root"
        );
        let created = ensure_drive(&registry, &home, "cap-ok").unwrap();
        assert_eq!(created, Some(drive_dir(&home, "cap-ok")));
    }

    #[test]
    fn a_captain_name_the_plugin_would_refuse_is_not_promoted() {
        let mut registry = Registry::default();
        for name in ["", "x".repeat(81).as_str(), "line\nbreak", "carriage\rreturn"] {
            let refused = registry.promote("cap-a", name);
            assert!(
                refused.is_err(),
                "the plugin refuses name {name:?}; promote accepted it"
            );
            assert!(
                registry.captains().is_empty(),
                "a refused promote must not create a record"
            );
        }
        registry.promote("cap-a", "A & B").unwrap();
        assert_eq!(registry.captains()[0].name, "A & B");
        registry.promote("cap-b", &"y".repeat(80)).unwrap();
    }

    /// The plugin measures a name with JS `String.length`, which counts UTF-16
    /// code units. An astral character is one `chars()` scalar but two code
    /// units, so the two predicates diverge from 41 astral characters on: 41
    /// scalars look legal here and 82 code units are refused there. This pins
    /// the boundary the two rules have to share.
    #[test]
    fn the_name_rule_counts_utf16_code_units_like_the_plugin() {
        // 40 astral characters are 40 scalars and 80 UTF-16 code units: the
        // plugin accepts the name, so a promote must accept it too.
        let at_the_limit = "\u{1F680}".repeat(40);
        assert_eq!(at_the_limit.chars().count(), 40);
        assert_eq!(at_the_limit.encode_utf16().count(), 80);
        let mut registry = Registry::default();
        registry
            .promote("cap-40", &at_the_limit)
            .expect("80 UTF-16 code units are inside the plugin's 1-80 rule");
        assert_eq!(registry.captains()[0].name, at_the_limit);

        // 41 astral characters are 41 scalars and 82 UTF-16 code units: the
        // plugin throws on the name, so the engine must refuse it rather than
        // stamp a row that takes that chat's every run down.
        let over_the_limit = "\u{1F680}".repeat(41);
        assert_eq!(
            over_the_limit.chars().count(),
            41,
            "the scalar count still looks legal"
        );
        assert_eq!(
            over_the_limit.encode_utf16().count(),
            82,
            "the plugin's count refuses it"
        );
        let refused = registry
            .promote("cap-41", &over_the_limit)
            .expect_err("82 UTF-16 code units break the plugin's 1-80 rule");
        assert!(
            refused.contains("UTF-16"),
            "the message must name the unit the rule counts: {refused}"
        );
        assert_eq!(
            registry.captains().len(),
            1,
            "a refused promote must not create a record"
        );
    }

    /// A crew row is checked on its **own** chat id as well as on its
    /// Captain's row: an id the plugin's id rule refuses must not be stamped
    /// and must not be given a Drive.
    #[test]
    fn a_crew_row_with_an_invalid_chat_id_is_refused() {
        let at = "2026-11-01T00:00:00Z".parse().unwrap();
        let registry = Registry {
            captains: vec![Captain {
                chat_id: "cap-ok".to_string(),
                name: "Grace".to_string(),
                created_at: at,
                archived: false,
                user_archived: false,
                crew: vec!["bad/id".to_string()],
                archived_crew: Vec::new(),
                crew_meta: BTreeMap::new(),
            }],
        };
        assert!(
            valid_captain_row("bad/id", "cap-ok", "Grace").is_err(),
            "the crew chat's own id is invalid"
        );
        assert!(
            captains_env(&registry, Path::new("/deepseek-home"), "bad/id").is_none(),
            "a crew chat with an invalid id was stamped"
        );
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        assert_eq!(
            ensure_drive(&registry, &home, "bad/id").unwrap(),
            None,
            "a crew chat with an invalid id was provisioned"
        );
        assert!(
            !captains_root(&home).exists(),
            "a refused crew row created something on disk"
        );
    }

    /// SUR-191: the row gate is one decision with two callers — the pre-check
    /// (`row`, read before `MUTATION`) and the commit re-read (`committed_row`,
    /// read under it). They must refuse the same three facts with the same
    /// words. This is a **pin, not a bug red**: it passes before and after the
    /// dedup, and it exists so the refactor cannot quietly change either
    /// message.
    #[test]
    fn the_row_gate_refuses_the_same_three_facts_on_both_paths() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Answers every read with the same row: the pre-check's path.
        struct Fixed(Option<ChatRow>);
        impl Chats for Fixed {
            fn chat(&self, _chat_id: &str) -> Option<ChatRow> {
                self.0.clone()
            }

            fn set_chat_archived(&self, _chat_id: &str, _archived: bool) -> Result<(), String> {
                Ok(())
            }
        }

        /// Answers the pre-check with a good row and the under-lock re-read with
        /// the violated one, so the commit's own gate is what refuses.
        struct Flips {
            after: Option<ChatRow>,
            calls: AtomicUsize,
        }
        impl Chats for Flips {
            fn chat(&self, _chat_id: &str) -> Option<ChatRow> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Some(ChatRow {
                        device_id: DEVICE.to_string(),
                        archived: false,
                        ..ChatRow::default()
                    })
                } else {
                    self.after.clone()
                }
            }

            fn set_chat_archived(&self, _chat_id: &str, _archived: bool) -> Result<(), String> {
                Ok(())
            }
        }

        fn refused(store: &CaptainsStore, chats: &dyn Chats, home: &Path, params: Value) -> String {
            let rpc = CaptainsRpc {
                store,
                chats,
                device_id: DEVICE,
                deepseek_home: home,
            };
            match rpc.call(methods::PROMOTE_CAPTAIN, &params) {
                Ok(_) => panic!("the row gate accepted a fact it must refuse"),
                Err(RpcError::BadParams(message)) => message,
                Err(other) => panic!("unexpected error: {other}"),
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let store = CaptainsStore::open(dir.path().to_path_buf());
        let params = json!({"chatId": "cap-a", "name": "Ada"});

        let facts: [(&str, Option<ChatRow>); 3] = [
            ("no such chat: cap-a", None),
            (
                "cap-a belongs to another device",
                Some(ChatRow {
                    device_id: "device-2".to_string(),
                    archived: false,
                    ..ChatRow::default()
                }),
            ),
            (
                "cap-a has a turn in flight",
                Some(ChatRow {
                    device_id: DEVICE.to_string(),
                    turn_in_flight: true,
                    archived: false,
                    ..ChatRow::default()
                }),
            ),
        ];
        for (message, violated) in facts {
            assert_eq!(
                refused(&store, &Fixed(violated.clone()), &home, params.clone()),
                message,
                "the pre-check's words"
            );
            assert_eq!(
                refused(
                    &store,
                    &Flips {
                        after: violated,
                        calls: AtomicUsize::new(0)
                    },
                    &home,
                    params.clone()
                ),
                message,
                "the commit re-read must refuse with the pre-check's words"
            );
        }
    }

    /// One staged bundle under `root`: `root/dsh-plugins/dsh-captains`.
    /// A bundle is `package.json` plus the entry `index.js`.
    fn bundle(root: &Path) -> PathBuf {
        let tree = root.join("dsh-plugins/dsh-captains");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("package.json"), b"{}").unwrap();
        std::fs::write(tree.join("index.js"), b"").unwrap();
        tree
    }

    /// SUR-107: the bundled plugin's directory, in the shipped order — the
    /// environment override, then `../Resources` beside the executable (the
    /// app-bundle layout), then the staged tree beside it.
    #[test]
    fn plugin_dir_prefers_the_override_then_the_packaged_tree() {
        let dir = tempfile::tempdir().unwrap();

        // A macOS-style tree: `Contents/Resources` beside `Contents/MacOS`,
        // with a staged `dsh-plugins` inside `Contents/MacOS` too, so the
        // order between the two exe-relative candidates is also pinned.
        let app = dir.path().join("Zeron.app");
        let resources = bundle(&app.join("Contents/Resources"));
        bundle(&app.join("Contents/MacOS"));

        // A tarball-style tree: `dsh-plugins/dsh-captains` beside the binary.
        let stage = dir.path().join("stage");
        let packaged = bundle(&stage);

        // The override wins even where a packaged tree would resolve.
        let override_dir = bundle(&dir.path().join("override-root"));
        let exe = stage.join("zeron");
        assert_eq!(
            plugin_dir_from(Some(&override_dir), &exe, "linux", true),
            Some(override_dir),
            "the environment override must win"
        );

        // Then `../Resources` beside the binary, before the staged tree in
        // the same directory.
        assert_eq!(
            plugin_dir_from(None, &app.join("Contents/MacOS/zeron"), "macos", true),
            Some(resources),
            "the app bundle's Resources tree is tried first"
        );

        // Then the staged tree beside the binary.
        assert_eq!(
            plugin_dir_from(None, &exe, "linux", true),
            Some(packaged),
            "the packaged tree beside the binary"
        );
    }

    /// SUR-107: the packaged root is per OS — the macOS app bundle's
    /// `Contents/Resources`, the Windows `beta/<version>` root above
    /// `engine`, and the Linux tarball's root beside the binary. The OS is
    /// an injected input, so all three run here on Linux.
    #[test]
    fn plugin_dir_mirrors_each_oss_packaged_layout() {
        let dir = tempfile::tempdir().unwrap();

        // macOS: `Contents/MacOS/zeron` -> `Contents/Resources/dsh-plugins/...`
        let app = dir.path().join("Zeron.app");
        let mac = bundle(&app.join("Contents/Resources"));
        let mac_exe = app.join("Contents/MacOS/zeron");
        assert_eq!(
            plugin_dir_from(None, &mac_exe, "macos", true),
            Some(mac),
            "the macOS app bundle's Resources"
        );

        // Windows: `beta/<version>/engine/zeron.exe` ->
        // `beta/<version>/dsh-plugins/...` (the root above `engine`).
        let version = dir.path().join("beta/1.2.3");
        let windows = bundle(&version);
        assert_eq!(
            plugin_dir_from(None, &version.join("engine/zeron.exe"), "windows", true),
            Some(windows),
            "the Windows beta/<version> root above engine"
        );

        // Linux: `<tarball>/zeron` -> `<tarball>/dsh-plugins/...`
        let stage = dir.path().join("tarball");
        let linux = bundle(&stage);
        assert_eq!(
            plugin_dir_from(None, &stage.join("zeron"), "linux", true),
            Some(linux),
            "the Linux tarball's root"
        );

        // The OS selects the layout — it is not a fallthrough: a Linux run
        // over the app-bundle exe must not read `Contents/Resources`. (Run
        // as a release here so the repository checkout below can't resolve
        // and mask the check.)
        assert_eq!(
            plugin_dir_from(None, &mac_exe, "linux", false),
            None,
            "a Linux run must not read Contents/Resources"
        );
    }

    /// SUR-107: an override that does not hold a bundle is `None` — the
    /// operator named the plugin it wants, so the lookup never falls through
    /// silently to a different tree.
    #[test]
    fn plugin_dir_is_none_when_the_override_holds_no_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        bundle(&stage);
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        assert_eq!(
            plugin_dir_from(Some(&empty), &stage.join("zeron"), "linux", true),
            None,
            "an override without a bundle is None, not a fallthrough"
        );
        assert_eq!(
            plugin_dir_from(
                Some(&dir.path().join("missing")),
                &stage.join("zeron"),
                "linux",
                true,
            ),
            None,
            "a nonexistent override is None too"
        );
    }

    /// SUR-107: with no override and no packaged tree, a dev run resolves the
    /// repository checkout — deterministic through `env!("CARGO_MANIFEST_DIR")`,
    /// which sits two directories below the checkout root.
    #[test]
    fn plugin_dir_falls_back_to_the_repo_checkout_on_a_dev_run() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("target/debug/zeron");
        let expected =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/dsh-captains");
        assert!(
            expected.join("package.json").is_file() && expected.join("index.js").is_file(),
            "this checkout must carry a complete plugins/dsh-captains: {}",
            expected.display()
        );
        assert_eq!(
            plugin_dir_from(None, &exe, "linux", true),
            Some(expected),
            "a debug run resolves the checkout"
        );
    }

    /// SUR-107: a release run resolves nothing from the repository checkout,
    /// even a checkout on the same disk as the binary — `env!` is baked at
    /// compile time, so without the gate a release build on a dev box would
    /// run unshipped plugin code.
    #[test]
    fn plugin_dir_ignores_the_repo_checkout_on_a_release_run() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("target/release/zeron");
        let expected =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/dsh-captains");
        assert!(
            expected.join("package.json").is_file() && expected.join("index.js").is_file(),
            "this checkout must carry a complete plugins/dsh-captains: {}",
            expected.display()
        );
        assert_eq!(
            plugin_dir_from(None, &exe, "linux", true),
            Some(expected),
            "the same binary at debug resolves the checkout"
        );
        assert_eq!(
            plugin_dir_from(None, &exe, "linux", false),
            None,
            "a release run must not resolve the repository checkout"
        );
    }

    /// SUR-107: a bundle is `package.json` *and* the entry file `index.js` —
    /// an override carrying only the manifest is `None`, not a path into
    /// nothing.
    #[test]
    fn plugin_dir_requires_the_entry_file() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join("override");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(override_dir.join("package.json"), b"{}").unwrap();
        let exe = dir.path().join("zeron");

        assert_eq!(
            plugin_dir_from(Some(&override_dir), &exe, "linux", true),
            None,
            "a bundle without index.js is not a plugin"
        );

        // The same directory resolves once `index.js` lands, so the `None`
        // above cannot be passing for the wrong reason.
        std::fs::write(override_dir.join("index.js"), b"").unwrap();
        assert_eq!(
            plugin_dir_from(Some(&override_dir), &exe, "linux", true),
            Some(override_dir),
            "with index.js written the same directory resolves"
        );
    }

    /// A minimal packaged Captains bundle for the provisioning-copy tests:
    /// the manifest must name the bundle, and the nested file proves the
    /// recursive copy and its hash cover more than the entry point.
    fn provisioning_source(root: &Path) -> PathBuf {
        let source = root.join("packaged");
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::write(
            source.join("package.json"),
            format!("{{\"name\":\"{BUNDLE_NAME}\"}}"),
        )
        .unwrap();
        std::fs::write(source.join("index.js"), b"first entry").unwrap();
        std::fs::write(source.join("src/tool.js"), b"first nested").unwrap();
        source
    }

    /// SUR-235: the fallback itself is portable to test: when the injected
    /// link primitive reports the Windows no-privilege case, provisioning
    /// materialises the bundle and marks the copy with the bundle's hash.
    #[test]
    fn a_failed_link_materialises_a_marked_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = provisioning_source(dir.path());
        let profile = dir.path().join("home/profiles/sdk");

        ensure_link_with(&profile, &source, |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no directory symlink privilege",
            ))
        })
        .unwrap();

        let entry = profile.join("node_modules").join(BUNDLE_NAME);
        assert!(
            std::fs::symlink_metadata(&entry)
                .expect("the materialised copy exists")
                .is_dir(),
            "the fallback leaves a real directory, not a link"
        );
        assert_eq!(
            std::fs::read(entry.join("src/tool.js")).unwrap(),
            b"first nested",
            "the fallback copies the bundle recursively"
        );
        assert_eq!(
            copy_marker_hash(&entry),
            Some(bundle_hash(&source).unwrap()),
            "the copy records the packaged bundle's hash"
        );
    }

    /// SUR-235: a copy made because the host has no link primitive is a
    /// byte-identical no-op while the packaged bundle is unchanged, and an
    /// upgrade refreshes that same disposable copy when the bundle's hash
    /// moves. The test drives `ensure_link` — provisioning's own decision
    /// point — rather than calling the copy primitive twice.
    #[test]
    fn a_materialised_copy_is_a_no_op_until_the_packaged_bundle_changes() {
        let dir = tempfile::tempdir().unwrap();
        let source = provisioning_source(dir.path());
        let profile = dir.path().join("home/profiles/sdk");
        let entry = profile.join("node_modules").join(BUNDLE_NAME);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        materialise_bundle(&source, &entry).unwrap();
        let first = [
            std::fs::read(entry.join("package.json")).unwrap(),
            std::fs::read(entry.join("index.js")).unwrap(),
            std::fs::read(entry.join("src/tool.js")).unwrap(),
            std::fs::read(entry.join(COPY_MARKER)).unwrap(),
        ];

        ensure_link(&profile, &source).unwrap();

        assert_eq!(
            [
                std::fs::read(entry.join("package.json")).unwrap(),
                std::fs::read(entry.join("index.js")).unwrap(),
                std::fs::read(entry.join("src/tool.js")).unwrap(),
                std::fs::read(entry.join(COPY_MARKER)).unwrap(),
            ],
            first,
            "an unchanged copy, including its marker, must stay byte-identical"
        );

        std::fs::write(source.join("src/tool.js"), b"second nested").unwrap();
        ensure_link(&profile, &source).unwrap();

        assert_eq!(
            std::fs::read(entry.join("src/tool.js")).unwrap(),
            b"second nested",
            "a changed packaged bundle refreshes the materialised copy"
        );
        assert_ne!(
            std::fs::read(entry.join(COPY_MARKER)).unwrap(),
            first[3],
            "the refreshed copy records the new bundle hash"
        );
        let refreshed = [
            std::fs::read(entry.join("package.json")).unwrap(),
            std::fs::read(entry.join("index.js")).unwrap(),
            std::fs::read(entry.join("src/tool.js")).unwrap(),
            std::fs::read(entry.join(COPY_MARKER)).unwrap(),
        ];
        ensure_link(&profile, &source).unwrap();
        assert_eq!(
            [
                std::fs::read(entry.join("package.json")).unwrap(),
                std::fs::read(entry.join("index.js")).unwrap(),
                std::fs::read(entry.join("src/tool.js")).unwrap(),
                std::fs::read(entry.join(COPY_MARKER)).unwrap(),
            ],
            refreshed,
            "the refreshed copy is itself a no-op on the next prepare"
        );
        for suffix in ["materialising", "retiring"] {
            assert!(
                !entry
                    .with_file_name(format!(".{BUNDLE_NAME}.{suffix}"))
                    .exists(),
                "the refresh leaves no .{suffix} tree behind"
            );
        }
    }

    /// Concurrent prepares of one stale copy are serialised: every caller
    /// either performs the refresh or waits and then sees the completed
    /// copy. None may delete another caller's half-written staging tree.
    #[test]
    fn overlapping_copy_refreshes_are_serialised() {
        let dir = tempfile::tempdir().unwrap();
        let source = provisioning_source(dir.path());
        let profile = dir.path().join("home/profiles/sdk");
        let entry = profile.join("node_modules").join(BUNDLE_NAME);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        materialise_bundle(&source, &entry).unwrap();
        std::fs::write(source.join("src/tool.js"), b"second nested").unwrap();

        let barrier = std::sync::Barrier::new(4);
        let barrier = &barrier;
        let profile = &profile;
        let source = &source;
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..4 {
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    ensure_link(profile, source)
                }));
            }
            for handle in handles {
                handle
                    .join()
                    .expect("a provisioning thread must not panic")
                    .unwrap();
            }
        });

        assert_eq!(
            std::fs::read(entry.join("src/tool.js")).unwrap(),
            b"second nested",
            "the completed refresh is intact after every prepare returns"
        );
        assert_eq!(
            copy_marker_hash(&entry),
            Some(bundle_hash(&source).unwrap()),
            "the surviving copy records the packaged bundle's hash"
        );
    }

    /// SUR-235: a real directory that names the bundle but carries no copy
    /// marker is a person's install even when its bytes differ from the
    /// packaged bundle. It still satisfies provisioning and is never
    /// rewritten as though it were our disposable copy.
    #[test]
    fn an_unmarked_real_bundle_directory_is_never_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let source = provisioning_source(dir.path());
        let profile = dir.path().join("home/profiles/sdk");
        let entry = profile.join("node_modules").join(BUNDLE_NAME);
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join("package.json"),
            format!("{{\"name\":\"{BUNDLE_NAME}\"}}"),
        )
        .unwrap();
        std::fs::write(entry.join("index.js"), b"the user's install").unwrap();
        std::fs::write(entry.join("keep-me"), b"user data").unwrap();

        ensure_link(&profile, &source).unwrap();

        assert_eq!(
            std::fs::read(entry.join("index.js")).unwrap(),
            b"the user's install",
            "an unmarked install is never overwritten"
        );
        assert_eq!(
            std::fs::read(entry.join("keep-me")).unwrap(),
            b"user data",
            "an unmarked install's own files survive"
        );
        assert!(
            !entry.join(COPY_MARKER).exists(),
            "provisioning must not retroactively claim a person's directory"
        );
    }

    // ── SUR-94: the archive cascade ───────────────────────────────────────
    // The cascade is not on this tree — `Captain` has no `userArchived` or
    // `archivedCrew` fields and `ArchiveCaptain` / `UnarchiveCaptain` /
    // `DeleteCaptain` are not methods — so these tests reach the methods by
    // their RPC names and read `captains.json` as plain JSON. Each fails
    // where the contract is missing: the unknown method first, then whatever
    // the file or the predicates still lack.

    /// `ArchiveCaptain` is a cascade: the record gains `userArchived` and
    /// every crew chat live at that moment is moved with it, the marker
    /// `archivedCrew` recording exactly those ids. `UnarchiveCaptain` wakes
    /// only what the archive itself moved: the flag clears, the spent marker
    /// goes, and the crew is live again — chat-c included, whose hand-set
    /// archive was never the cascade's business.
    #[test]
    fn captain_archive_sleeps_the_whole_crew_and_unarchive_wakes_only_what_it_moved() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None)
                .row("chat-c", Some("space-1"), None)
                .archived("chat-c"),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();
        // chat-c crews on record with a row already archived by hand —
        // live, but the flag is the person's own, which the cascade's
        // live check must leave alone (it joins straight on the registry,
        // as crewing it through the RPC is not the fixture's business).
        // A live crew chat beside a hand-archived one is the pair the
        // marker must split.
        rig.store.add_crew("cap-a", "chat-c").unwrap();

        let answer = rig
            .call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain is one of the Captain methods");
        assert_eq!(
            entry(&answer, "cap-a").expect("the record is kept")["userArchived"],
            true,
            "the archive answers the roster, flagged"
        );

        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert_eq!(record["userArchived"], true, "the flag is persisted");
        assert_eq!(
            record["archived"], false,
            "`archived` stays promote/demote's own flag — the cascade never writes it"
        );
        assert_eq!(
            record["archivedCrew"],
            json!(["chat-b"]),
            "the marker records exactly the chats the archive moved — never chat-c"
        );
        assert_eq!(
            record["crew"],
            json!(["chat-b", "chat-c"]),
            "the crew list itself is kept"
        );
        let snapshot = rig.store.snapshot();
        assert!(
            !snapshot.is_captain("cap-a"),
            "a userArchived Captain is not live"
        );
        assert!(
            snapshot.captain_of_crew("chat-b").is_none(),
            "and its crew is not live either"
        );

        rig.call("UnarchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("UnarchiveCaptain is one of the Captain methods");
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert_eq!(record["userArchived"], false, "the flag is cleared");
        assert!(
            record.get("archivedCrew").is_none(),
            "the spent marker is cleared, not kept"
        );
        let snapshot = rig.store.snapshot();
        assert!(snapshot.is_captain("cap-a"), "the Captain is live again");
        assert_eq!(
            snapshot.captain_of_crew("chat-b").unwrap().chat_id,
            "cap-a",
            "chat-b wakes with it"
        );
        assert_eq!(
            snapshot.captain_of_crew("chat-c").unwrap().chat_id,
            "cap-a",
            "chat-c's membership never left; its hand-set flag stays untouched"
        );
    }

    /// SUR-253: a hidden Captain's crew is not live, so a member can be added
    /// to a second live crew while it sleeps. Waking the first must reconcile
    /// like `promote` does — the member ends up in exactly one live crew —
    /// instead of handing it back to two.
    #[test]
    fn an_unarchive_does_not_leave_a_member_in_two_live_crews() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("cap-b", Some("space-1"), None)
                .row("chat-x", Some("space-1"), None),
        );
        for (chat_id, name) in [("cap-a", "Ada"), ("cap-b", "Grace")] {
            rig.call(
                methods::PROMOTE_CAPTAIN,
                json!({"chatId": chat_id, "name": name}),
            )
            .unwrap();
        }
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-x"}),
        )
        .unwrap();

        // cap-a sleeps with chat-x; the member is no longer in a live crew,
        // so cap-b can take it before cap-a is woken.
        rig.call(methods::ARCHIVE_CAPTAIN, json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain is one of the Captain methods");
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-b", "chatId": "chat-x"}),
        )
        .expect("a hidden Captain's crew does not hold the chat");

        rig.call(methods::UNARCHIVE_CAPTAIN, json!({"chatId": "cap-a"}))
            .expect("UnarchiveCaptain is one of the Captain methods");
        let roster = rig.roster();
        assert!(
            crew(&roster, "cap-a").is_empty(),
            "the woken Captain keeps only crew that is still free: {:?}",
            crew(&roster, "cap-a")
        );
        assert_eq!(
            crew(&roster, "cap-b"),
            vec!["chat-x".to_string()],
            "the chat stays in the live crew it joined"
        );
    }

    /// The same reconcile, other half: a kept member that became a live
    /// Captain while its Captain slept cannot come back as crew.
    #[test]
    fn an_unarchive_drops_a_kept_crew_member_that_became_a_live_captain() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-x", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-x"}),
        )
        .unwrap();
        rig.call(methods::ARCHIVE_CAPTAIN, json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain is one of the Captain methods");
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "chat-x", "name": "Xena"}),
        )
        .expect("a hidden Captain's crew is free to become a Captain");

        rig.call(methods::UNARCHIVE_CAPTAIN, json!({"chatId": "cap-a"}))
            .expect("UnarchiveCaptain is one of the Captain methods");
        let roster = rig.roster();
        assert!(
            crew(&roster, "cap-a").is_empty(),
            "a live Captain cannot be crew: {:?}",
            crew(&roster, "cap-a")
        );
        assert_eq!(
            entry(&roster, "chat-x").expect("chat-x is a Captain")["archived"],
            false,
            "and stays a Captain"
        );
    }

    /// A crew id with no row at all is not the cascade's business either: it
    /// is skipped, never written into the marker, and neither archive nor
    /// unarchive touches it. The row that does exist beside it is the moved
    /// one, so the marker is `["chat-b"]` and nobody else.
    #[test]
    fn captain_archive_skips_a_crew_id_with_no_row_and_never_marks_it() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();
        // A crew id the host has no row for — the fixture stands in for a
        // row that is gone entirely, which the cascade can neither archive
        // nor later wake.
        rig.store.add_crew("cap-a", "chat-gone").unwrap();

        rig.call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain is one of the Captain methods");
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert_eq!(
            record["archivedCrew"],
            json!(["chat-b"]),
            "only the row that exists is marked: chat-gone is skipped"
        );

        rig.call("UnarchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("UnarchiveCaptain is one of the Captain methods");
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert!(
            record.get("archivedCrew").is_none(),
            "the marker spends whole; chat-gone was never in it"
        );
        assert_eq!(
            record["crew"],
            json!(["chat-b", "chat-gone"]),
            "and the membership is untouched: skipping a row is not removing it"
        );
    }

    /// Both halves of the cascade are safe to retry: an archive of an
    /// already-`userArchived` Captain moves nothing new — no commit, and no
    /// second copy in the marker — and an unarchive of a Captain that is not
    /// `userArchived` commits nothing either.
    #[test]
    fn captain_archive_twice_moves_nothing_the_second_time() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();

        rig.call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain is one of the Captain methods");
        let committed = rig.store.commits();
        let saved = rig.file();

        rig.call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("a retried archive is still answered");
        assert_eq!(
            rig.store.commits(),
            committed,
            "the second archive moves nothing"
        );
        assert_eq!(rig.file(), saved, "and writes nothing");
        let after: Value = serde_json::from_slice(&rig.file()).unwrap();
        assert_eq!(
            entry(&after, "cap-a").unwrap()["archivedCrew"],
            json!(["chat-b"]),
            "the retry grew no marker"
        );

        rig.call("UnarchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("UnarchiveCaptain is one of the Captain methods");
        let committed = rig.store.commits();
        let saved = rig.file();

        // A Captain that is not userArchived has no marker to spend: the
        // retried unarchive is the live-Captain no-op.
        rig.call("UnarchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("a retried unarchive is still answered");
        assert_eq!(
            rig.store.commits(),
            committed,
            "an unarchive with nothing to spend commits nothing"
        );
        assert_eq!(rig.file(), saved, "and writes nothing");
        let after: Value = serde_json::from_slice(&rig.file()).unwrap();
        assert!(
            entry(&after, "cap-a").unwrap().get("archivedCrew").is_none(),
            "and the marker stays cleared"
        );
    }

    /// `DeleteCaptain` drops the registry record and nothing else: the chats
    /// it names keep their rows — the cascade never touches them — and the
    /// Drive folder is the caller's to keep. What the registry can pin is
    /// that the record is gone and its neighbour is whole.
    #[test]
    fn captain_delete_drops_the_record_and_keeps_its_chats() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let file = dir.path().join("captains.json");
        // Two records on file: cap-a's, as archive-then-demote leaves it —
        // both flags set, its spent marker kept — and a live cap-b with its
        // crew. Nothing on this tree writes `userArchived`, so the record is
        // seeded by hand.
        std::fs::write(
            &file,
            r#"{"captains":[{"chatId":"cap-a","name":"Ada","createdAt":"2026-11-01T00:00:00Z","archived":true,"userArchived":true,"crew":["chat-b"],"archivedCrew":["chat-b"]},{"chatId":"cap-b","name":"Grace","createdAt":"2026-11-01T00:00:00Z","archived":false,"crew":["chat-x"]}]}"#,
        )
        .unwrap();
        let store = CaptainsStore::open(dir.path().to_path_buf());
        let chats = FakeChats::default()
            .row("cap-a", Some("space-1"), None)
            .row("cap-b", Some("space-1"), None)
            .row("chat-b", Some("space-1"), None)
            .row("chat-x", Some("space-1"), None);
        let rpc = CaptainsRpc {
            store: &store,
            chats: &chats,
            device_id: DEVICE,
            deepseek_home: &home,
        };

        let answer = rpc
            .call("DeleteCaptain", &json!({"chatId": "cap-a"}))
            .expect("DeleteCaptain is one of the Captain methods");
        assert!(
            entry(&answer, "cap-a").is_none(),
            "the answer's roster has no row for it"
        );
        assert!(entry(&answer, "cap-b").is_some(), "cap-b's row answers still");

        let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let captains = saved["captains"].as_array().unwrap();
        assert_eq!(captains.len(), 1, "the one record dropped, the other kept");
        let cap_b = entry(&saved, "cap-b").expect("cap-b's record is kept");
        assert_eq!(cap_b["crew"], json!(["chat-x"]), "its crew intact");
        assert_eq!(cap_b["archived"], false, "and still live");

        let snapshot = store.snapshot();
        assert!(!snapshot.is_captain("cap-a"));
        assert!(snapshot.is_captain("cap-b"), "the other Captain stays live");
        // chat-b was never a registry fact the delete could touch: its row
        // lives in the workspace doc and the Drive in the caller's hands —
        // both stay, and there is nothing here to count.
    }

    /// A `captains.json` written before the cascade has no `userArchived` or
    /// `archivedCrew` key: it must still load — both fields are
    /// `#[serde(default)]` — and an unarchive of such a record is a
    /// committed no-op, not an error: there is no marker to spend.
    #[test]
    fn a_registry_file_without_the_marker_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("deepseek");
        std::fs::create_dir(&home).unwrap();
        let file = dir.path().join("captains.json");
        // The pre-SUR-94 record: every key the file has ever had.
        std::fs::write(
            &file,
            r#"{"captains":[{"chatId":"cap-a","name":"Ada","createdAt":"2026-11-01T00:00:00Z","archived":false,"crew":["chat-b"]}]}"#,
        )
        .unwrap();
        let registry = Registry::load(dir.path()).unwrap();
        assert!(
            registry.is_captain("cap-a"),
            "the old file loads as a live Captain"
        );
        assert_eq!(
            registry.captain_of_crew("chat-b").unwrap().chat_id,
            "cap-a"
        );

        let store = CaptainsStore::open(dir.path().to_path_buf());
        let chats = FakeChats::default().row("cap-a", Some("space-1"), None);
        let rpc = CaptainsRpc {
            store: &store,
            chats: &chats,
            device_id: DEVICE,
            deepseek_home: &home,
        };
        let before = std::fs::read(&file).unwrap();
        rpc.call("UnarchiveCaptain", &json!({"chatId": "cap-a"}))
            .expect("an unarchive with no marker to spend is a no-op, not an error");
        assert_eq!(store.commits(), 0, "nothing moved, nothing committed");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            before,
            "and nothing written"
        );
    }

    /// The two flags' boundary: `archived` — promote/demote's own — refuses
    /// the archive, because a demoted Captain can never be hidden; while
    /// `userArchived` alone is already not-live for the stamping predicates
    /// (`is_captain`, `captain_of_crew`, `run_role`: the SUR-80/SUR-106
    /// reads). Neither flag may write the other.
    #[test]
    fn a_demoted_captain_cannot_be_hidden_and_a_user_archived_one_is_not_live() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();
        rig.call(methods::DEMOTE_CAPTAIN, json!({"chatId": "cap-a"}))
            .unwrap();
        let before = rig.file();

        // Archive is refused on a demoted record — as bad params, the
        // registry's own refusal kind, not a silent no-op.
        match rig.call("ArchiveCaptain", json!({"chatId": "cap-a"})) {
            Err(RpcError::BadParams(_)) => {}
            other => panic!(
                "a demoted Captain must refuse the archive as bad params, got {other:?}"
            ),
        }
        assert_eq!(rig.file(), before, "the refused archive writes nothing");

        // A live record carrying `userArchived` alone — the file as an
        // archive leaves it, seeded by hand because nothing on this tree
        // produces one yet.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("captains.json"),
            r#"{"captains":[{"chatId":"cap-a","name":"Ada","createdAt":"2026-11-01T00:00:00Z","archived":false,"userArchived":true,"crew":["chat-b"],"archivedCrew":["chat-b"]}]}"#,
        )
        .unwrap();
        let registry = Registry::load(dir.path()).unwrap();
        assert!(
            !registry.is_captain("cap-a"),
            "a userArchived Captain is not live: is_captain reads archived || userArchived"
        );
        assert!(
            registry.captain_of_crew("chat-b").is_none(),
            "and its crew is not stamped"
        );
        assert_eq!(registry.run_role("cap-a"), RunRole::None);
        assert_eq!(registry.run_role("chat-b"), RunRole::None);
        let home = Path::new("/deepseek-home");
        assert!(
            captains_env(&registry, home, "cap-a").is_none(),
            "no stamp for a hidden Captain"
        );
        assert!(
            captains_env(&registry, home, "chat-b").is_none(),
            "nor for its crew"
        );
    }
    /// The cascade's whole point — the crew goes to sleep — is a write to the
    /// chat rows, and this fake records every one in order. Exactly the live
    /// crew chat is archived: a chat the person had already archived by hand
    /// is not the cascade's to move, and the Captain's own row is never one of
    /// them. Unarchive wakes exactly the id the marker kept.
    #[test]
    fn the_archive_writes_the_live_crew_chats_and_the_unarchive_wakes_only_them() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None)
                .row("chat-c", Some("space-1"), None)
                .archived("chat-c"),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();
        // chat-c crews on record but was archived by hand on another device, so
        // the roster rule never let it in through AddCrew — it joins straight on
        // the registry, the way a pre-existing crew member survives one.
        rig.store.add_crew("cap-a", "chat-c").unwrap();

        rig.call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain is one of the Captain methods");
        assert_eq!(
            *rig.chats.writes.borrow(),
            vec![("chat-b".to_string(), true)],
            "the live crew chat is archived, the hand-archived one is not, and the Captain never is"
        );
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        assert_eq!(
            entry(&saved, "cap-a").unwrap()["archivedCrew"],
            json!(["chat-b"]),
            "the marker names exactly the ids the write moved"
        );

        rig.call("UnarchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("UnarchiveCaptain is one of the Captain methods");
        assert_eq!(
            *rig.chats.writes.borrow(),
            vec![
                ("chat-b".to_string(), true),
                ("chat-b".to_string(), false),
            ],
            "unarchive wakes the id the marker kept, in that order"
        );
        assert!(
            rig.chats
                .rows
                .get("chat-c")
                .is_some_and(|row| row.archived),
            "a chat archived by hand is still archived after the Captain wakes"
        );
    }

    /// A chat write that fails must not strand half a crew: the marker and the
    /// flag are committed first, so the retry finishes the cascade instead of
    /// finding nothing to do.
    #[test]
    fn a_partly_failed_archive_finishes_on_retry() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();

        rig.chats.refuse.set(1);
        match rig.call("ArchiveCaptain", json!({"chatId": "cap-a"})) {
            Err(RpcError::Failed(_)) => {}
            other => panic!("a refused chat write must surface as a failure, got {other:?}"),
        }
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert_eq!(record["userArchived"], true, "the flag landed before the write");
        assert_eq!(
            record["archivedCrew"],
            json!(["chat-b"]),
            "and so did the marker, which is what makes the retry possible"
        );

        rig.call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("the retry is answered");
        assert_eq!(
            rig.chats.writes.borrow().last(),
            Some(&("chat-b".to_string(), true)),
            "the retry re-applies the marker's id: {:?}",
            rig.chats.writes.borrow()
        );
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        assert_eq!(
            entry(&saved, "cap-a").unwrap()["archivedCrew"],
            json!(["chat-b"]),
            "and grows no second marker"
        );
    }

    /// The other direction has the same rule: the marker is spent only after
    /// the chats it names are actually awake, so a failed write leaves the
    /// record still hidden and still carrying the ids a retry needs. Spending
    /// the marker first would lose them silently and leave the crew asleep.
    #[test]
    fn a_partly_failed_unarchive_finishes_on_retry() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();
        rig.call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain");

        rig.chats.refuse.set(1);
        match rig.call("UnarchiveCaptain", json!({"chatId": "cap-a"})) {
            Err(RpcError::Failed(_)) => {}
            other => panic!("a refused chat write must surface as a failure, got {other:?}"),
        }
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert_eq!(
            record["userArchived"], true,
            "a failed wake leaves the Captain hidden, not half-awake"
        );
        assert_eq!(
            record["archivedCrew"],
            json!(["chat-b"]),
            "and the marker survives, so the retry knows what to wake"
        );

        rig.call("UnarchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("the retry is answered");
        assert_eq!(
            rig.chats.writes.borrow().last(),
            Some(&("chat-b".to_string(), false)),
            "the retry wakes the marker's id: {:?}",
            rig.chats.writes.borrow()
        );
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert_eq!(record["userArchived"], false, "and the Captain is live again");
        assert!(
            record.get("archivedCrew").is_none(),
            "with the marker spent once the wake actually landed"
        );
    }

    /// A demoted record's reactivation may only drop crew members a **live**
    /// Captain has since claimed. A hidden (`userArchived`) Captain is not
    /// live — its crew is not stamped and it blocks nothing — so the member
    /// it names is free and the reactivation keeps it.
    #[test]
    fn a_promote_keeps_a_crew_member_that_only_a_hidden_captain_names() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("cap-b", Some("space-1"), None)
                .row("chat-c", Some("space-1"), None),
        );
        // A is demoted while naming chat-c. B then takes chat-c — legal,
        // because A is not live — and is hidden, so only a non-live Captain
        // names the member.
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-c"}),
        )
        .unwrap();
        rig.call(methods::DEMOTE_CAPTAIN, json!({"chatId": "cap-a"}))
            .unwrap();
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-b", "name": "Grace"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-b", "chatId": "chat-c"}),
        )
        .unwrap();
        rig.call("ArchiveCaptain", json!({"chatId": "cap-b"}))
            .expect("ArchiveCaptain hides B and archives its crew");

        let roster = rig
            .call(
                methods::PROMOTE_CAPTAIN,
                json!({"chatId": "cap-a", "name": "Ada"}),
            )
            .unwrap();
        assert_eq!(
            crew(&roster, "cap-a"),
            vec!["chat-c".to_string()],
            "a hidden Captain names chat-c but is not live, so chat-c is free"
        );
    }

    /// Demoting a hidden Captain is refused as bad params — the same kind as
    /// `ArchiveCaptain` on a demoted record. It is not a no-op: it would
    /// create the demoted-and-hidden state the archive refuses to create.
    #[test]
    fn a_hidden_captain_cannot_be_demoted() {
        let rig = Rig::new(
            FakeChats::default()
                .row("cap-a", Some("space-1"), None)
                .row("chat-b", Some("space-1"), None),
        );
        rig.call(
            methods::PROMOTE_CAPTAIN,
            json!({"chatId": "cap-a", "name": "Ada"}),
        )
        .unwrap();
        rig.call(
            methods::ADD_CREW,
            json!({"captainId": "cap-a", "chatId": "chat-b"}),
        )
        .unwrap();
        rig.call("ArchiveCaptain", json!({"chatId": "cap-a"}))
            .expect("ArchiveCaptain");
        let before = rig.file();

        bad_params(
            rig.call(methods::DEMOTE_CAPTAIN, json!({"chatId": "cap-a"})),
            "hidden",
        );
        assert_eq!(rig.file(), before, "the refused demote writes nothing");
        let saved: Value = serde_json::from_slice(&rig.file()).unwrap();
        let record = entry(&saved, "cap-a").unwrap();
        assert_eq!(record["archived"], false, "a hidden Captain is not demoted");
        assert_eq!(record["userArchived"], true, "and stays hidden");
    }

    struct FakeMergeRunner {
        target: Mutex<String>,
        base: Mutex<String>,
        state: Mutex<String>,
        pr_head: Mutex<String>,
        remote_head: Mutex<String>,
        files: Vec<String>,
        on_rebase: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
        active: std::sync::atomic::AtomicUsize,
        max_active: std::sync::atomic::AtomicUsize,
        rebase_calls: std::sync::atomic::AtomicUsize,
        release: std::sync::atomic::AtomicUsize,
        land_calls: std::sync::atomic::AtomicUsize,
        move_target: std::sync::atomic::AtomicBool,
    }

    impl FakeMergeRunner {
        fn new(files: &[&str]) -> Self {
            Self {
                target: Mutex::new("a".repeat(40)),
                base: Mutex::new("main".into()),
                state: Mutex::new("OPEN".into()),
                pr_head: Mutex::new("c".repeat(40)),
                remote_head: Mutex::new("c".repeat(40)),
                files: files.iter().map(|file| file.to_string()).collect(),
                on_rebase: Mutex::new(None),
                active: std::sync::atomic::AtomicUsize::new(0),
                max_active: std::sync::atomic::AtomicUsize::new(0),
                rebase_calls: std::sync::atomic::AtomicUsize::new(0),
                release: std::sync::atomic::AtomicUsize::new(0),
                land_calls: std::sync::atomic::AtomicUsize::new(0),
                move_target: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl MergeRunner for FakeMergeRunner {
        fn fetch(&self, _repo: &Path, _branch: &str) -> Result<(), String> {
            Ok(())
        }

        fn target_sha(&self, _repo: &Path) -> Result<String, String> {
            Ok(self.target.lock().unwrap().clone())
        }

        fn branch_sha(&self, _repo: &Path, _branch: &str) -> Result<String, String> {
            Ok(self.remote_head.lock().unwrap().clone())
        }

        fn pull_request_base(&self, _repo: &Path, _pr: u64) -> Result<String, String> {
            Ok(self.base.lock().unwrap().clone())
        }

        fn pull_request_state(&self, _repo: &Path, _pr: u64) -> Result<String, String> {
            Ok(self.state.lock().unwrap().clone())
        }

        fn pull_request_head(&self, _repo: &Path, _pr: u64) -> Result<String, String> {
            Ok(self.pr_head.lock().unwrap().clone())
        }

        fn changed_files(&self, _repo: &Path, _base: &str, _head: &str) -> Result<Vec<String>, String> {
            Ok(self.files.clone())
        }

        fn file_exists_at(&self, _repo: &Path, _head: &str, _path: &str) -> Result<bool, String> {
            Ok(true)
        }

        fn rebase_gate_publish(
            &self,
            _repo: &Path,
            _branch: &str,
            head: &str,
            _lease: &str,
            _scratch: &Path,
            before_publish: &dyn Fn(&str) -> Result<(), String>,
        ) -> Result<String, String> {
            use std::sync::atomic::Ordering;
            let call = self.rebase_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            while self.release.load(Ordering::SeqCst) < call {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            if let Some(callback) = &*self.on_rebase.lock().unwrap() {
                callback();
            }
            if self.move_target.swap(false, Ordering::SeqCst) {
                *self.target.lock().unwrap() = "b".repeat(40);
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            let mut bytes = head.as_bytes().to_vec();
            bytes[39] = b'0' + (call as u8 % 10);
            let new_head = String::from_utf8(bytes).unwrap();
            before_publish(&new_head)?;
            Ok(new_head)
        }

        fn land(&self, _repo: &Path, _pr: u64, _head: &str) -> Result<String, String> {
            self.land_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("merged".into())
        }
    }

    fn ready_receipt(drive: &Path, crew: &str, repo: &Path) {
        let ready = drive.join("state").join(MERGE_READY_DIR);
        std::fs::create_dir_all(&ready).unwrap();
        let receipt = ReadyReceipt {
            crew_id: crew.into(),
            repo: repo.to_string_lossy().into_owned(),
            pr: 100,
            branch: format!("crew/{crew}"),
            head_sha: "c".repeat(40),
            slug: format!("{crew}-change"),
        };
        std::fs::write(
            ready.join(format!("{crew}.json")),
            serde_json::to_string(&receipt).unwrap(),
        )
        .unwrap();
    }

    fn outcome_at(drive: &Path, crew: &str) -> MergeOutcome {
        let text = std::fs::read_to_string(
            drive
                .join("state")
                .join(MERGE_QUEUE_DIR)
                .join(MERGE_RESULTS_DIR)
                .join(format!("{crew}.json")),
        )
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Two scanners claiming two ready receipts at once contend for one merge
    /// mutex: while the first gate is parked, the second cannot enter its own
    /// gate. Both then land in turn rather than racing the shared checkout,
    /// version stamp or PR transition.
    #[test]
    fn helm_merge_queue_concurrent_ready_receipts_serialize() {
        use std::sync::atomic::Ordering;
        let root = tempfile::tempdir().unwrap();
        let drive1 = root.path().join("captains").join("cap-a");
        let drive2 = root.path().join("captains").join("cap-b");
        std::fs::create_dir_all(&drive1).unwrap();
        std::fs::create_dir_all(&drive2).unwrap();
        let repo = tempfile::tempdir().unwrap();
        // Only the first receipt exists before the first scan. The second is
        // written while that scan is parked inside its gate, so the second
        // scanner is guaranteed to be the one to claim it. Without the shared
        // Captain-root mutex it would enter the gate immediately and make
        // `rebase_calls` two.
        ready_receipt(&drive1, "crew-a", repo.path());
        let runner = Arc::new(FakeMergeRunner::new(&[
            "changelog.d/crew-a-change.md",
            "changelog.d/crew-b-change.md",
        ]));
        let first = {
            let drive_path = drive1.clone();
            let runner = runner.clone();
            std::thread::spawn(move || process_ready_receipts_with(&drive_path, runner.as_ref(), None))
        };
        while runner.rebase_calls.load(Ordering::SeqCst) < 1 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        ready_receipt(&drive2, "crew-b", repo.path());
        let second = {
            let drive_path = drive2.clone();
            let runner = runner.clone();
            std::thread::spawn(move || process_ready_receipts_with(&drive_path, runner.as_ref(), None))
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(
            runner.rebase_calls.load(Ordering::SeqCst),
            1,
            "a second ready receipt entered the merge gate while the mutex was held"
        );
        assert_eq!(runner.active.load(Ordering::SeqCst), 1);
        runner.release.store(1, Ordering::SeqCst);
        while runner.rebase_calls.load(Ordering::SeqCst) < 2 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        runner.release.store(2, Ordering::SeqCst);
        let outcomes: Vec<_> = [first, second]
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            outcomes.iter().flatten().count(),
            2,
            "both ready receipts are settled exactly once"
        );
        assert_eq!(runner.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 2);
        assert_eq!(outcome_at(&drive1, "crew-a").status, MergeOutcomeStatus::Landed);
        assert_eq!(outcome_at(&drive2, "crew-b").status, MergeOutcomeStatus::Landed);
    }

    /// The queue rechecks `origin/main` after the local gate. A target branch
    /// moving underneath the gate aborts before `gh pr merge`; the rebased PR
    /// head is never matched against a stale main.
    #[test]
    fn helm_merge_queue_aborts_when_the_target_head_moves() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        runner.release.store(1, Ordering::SeqCst);
        runner.move_target.store(true, Ordering::SeqCst);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Aborted);
        assert!(outcomes[0].reason.contains("target head moved"), "{}", outcomes[0].reason);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 0);
    }

    /// A receipt is not valid merely because it sits in a Captain's Drive:
    /// its crewId must be a live member of that Captain's crew. This keeps a
    /// mistyped or stale receipt from landing an unrelated crew branch.
    #[test]
    fn helm_merge_queue_rejects_a_receipt_outside_the_live_crew() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        ready_receipt(drive.path(), "crew-b", repo.path());
        let mut registry = Registry::default();
        registry.promote("cap-a", "Ada").unwrap();
        registry.add_crew("cap-a", "crew-a").unwrap();
        let runner = FakeMergeRunner::new(&[
            "changelog.d/crew-a-change.md",
            "changelog.d/crew-b-change.md",
        ]);
        runner.release.store(99, Ordering::SeqCst);
        let live_crew = |crew_id: &str| {
            registry
                .captain_of_crew(crew_id)
                .is_some_and(|captain| captain.chat_id == "cap-a" && captain.live())
        };
        let outcomes = process_ready_receipts_with(drive.path(), &runner, Some(&live_crew));
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcome_at(drive.path(), "crew-a").status, MergeOutcomeStatus::Landed);
        let outsider = outcome_at(drive.path(), "crew-b");
        assert_eq!(outsider.status, MergeOutcomeStatus::Failed);
        assert!(outsider.reason.contains("live member"), "{}", outsider.reason);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 1);
    }

    /// The lock metadata format once put the timestamp after the PID on one
    /// line. Even that legacy form must still parse as a live owner rather
    /// than expire underneath a long local test run.
    #[test]
    fn helm_merge_queue_lock_owner_survives_timestamp_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join(MERGE_LOCK_FILE);
        std::fs::write(&lock, format!("pid={} created=now\n", std::process::id())).unwrap();
        assert!(MergeMutexGuard::owner_alive(&lock));
    }

    /// A claim already present when a scan acquires the mutex cannot be
    /// active in another scanner: every scan performs all claim processing
    /// under that same mutex. It is therefore a crash-stranded receipt and is
    /// settled instead of waiting forever.
    #[test]
    fn helm_merge_queue_recovers_a_crash_stranded_claim() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let claims = drive
            .path()
            .join("state")
            .join(MERGE_QUEUE_DIR)
            .join(MERGE_CLAIMS_DIR);
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::rename(
            drive.path().join("state").join(MERGE_READY_DIR).join("crew-a.json"),
            claims.join("crew-a.json"),
        )
        .unwrap();
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        runner.release.store(1, Ordering::SeqCst);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Landed);
        assert!(!claims.join("crew-a.json").exists());
    }

    /// Crew membership is checked again after the expensive gate. Removing
    /// the member while its tests run withdraws the merge instead of landing
    /// a branch for a crew the user has already removed.
    #[test]
    fn helm_merge_queue_revalidates_live_crew_before_landing() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        runner.release.store(1, Ordering::SeqCst);
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        *runner.on_rebase.lock().unwrap() = Some(Box::new({
            let live = live.clone();
            move || live.store(false, Ordering::SeqCst)
        }));
        let live_crew = |_: &str| live.load(Ordering::SeqCst);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, Some(&live_crew));
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Failed);
        assert!(outcomes[0].reason.contains("removed"), "{}", outcomes[0].reason);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 0);
    }

    /// A published rebased head is durable merge progress. Recovery resumes
    /// after publication with that exact head instead of rebasing again or
    /// forcing over the queue's own update.
    #[test]
    fn helm_merge_queue_resumes_a_published_rebase() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let scratch = drive
            .path()
            .join("state")
            .join(MERGE_QUEUE_DIR)
            .join("crew-a");
        let rebased = "d".repeat(40);
        write_merge_progress(
            &scratch.join("progress.json"),
            &"c".repeat(40),
            &rebased,
            &"a".repeat(40),
            &[],
        )
        .unwrap();
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        *runner.remote_head.lock().unwrap() = rebased.clone();
        runner.release.store(1, Ordering::SeqCst);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Landed);
        assert_eq!(outcomes[0].landed_head.as_deref(), Some(rebased.as_str()));
        assert_eq!(runner.rebase_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 1);
    }

    /// A self-published head does not make the receipt unrecoverable when
    /// main moved before the first landing attempt. Because the progress file
    /// proves that tip is ours, recovery may re-rebase the original head and
    /// lease exactly that stale tip.
    #[test]
    fn helm_merge_queue_rerebases_its_own_stale_publication() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let scratch = drive
            .path()
            .join("state")
            .join(MERGE_QUEUE_DIR)
            .join("crew-a");
        let stale = "d".repeat(40);
        write_merge_progress(
            &scratch.join("progress.json"),
            &"c".repeat(40),
            &stale,
            &"a".repeat(40),
            &[],
        )
        .unwrap();
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        *runner.remote_head.lock().unwrap() = stale;
        *runner.target.lock().unwrap() = "e".repeat(40);
        runner.release.store(1, Ordering::SeqCst);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Landed);
        assert_eq!(runner.rebase_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 1);
    }

    /// A merged PR is not automatic success: if its final head is neither
    /// the reviewed commit nor one of this queue's recorded rebased heads, a
    /// maintainer merged a different revision and the receipt must fail.
    #[test]
    fn helm_merge_queue_rejects_an_unreviewed_already_merged_head() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        *runner.state.lock().unwrap() = "MERGED".into();
        *runner.pr_head.lock().unwrap() = "f".repeat(40);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Failed);
        assert!(outcomes[0].reason.contains("unreviewed head"), "{}", outcomes[0].reason);
        assert_eq!(runner.rebase_calls.load(Ordering::SeqCst), 0);
    }

    /// When a replacement push is interrupted, the branch can remain on an
    /// older queue-published head while progress names the newer candidate.
    /// The older head is still known-safe progress and must be leasable for
    /// the next rebase rather than rejected as foreign.
    #[test]
    fn helm_merge_queue_recovers_an_interrupted_replacement_push() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let scratch = drive
            .path()
            .join("state")
            .join(MERGE_QUEUE_DIR)
            .join("crew-a");
        let previous = "d".repeat(40);
        write_merge_progress(
            &scratch.join("progress.json"),
            &"c".repeat(40),
            &"e".repeat(40),
            &"a".repeat(40),
            std::slice::from_ref(&previous),
        )
        .unwrap();
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        *runner.remote_head.lock().unwrap() = previous;
        runner.release.store(1, Ordering::SeqCst);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Landed);
        assert_eq!(runner.rebase_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 1);
    }

    /// Recovery is idempotent across the most dangerous crash boundary: the
    /// merge command landed, then the engine died before writing the result.
    /// The replayed receipt sees the merged PR and settles successfully
    /// without requiring the deleted crew branch or rerunning the gate.
    #[test]
    fn helm_merge_queue_recovers_an_already_merged_pull_request() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        *runner.state.lock().unwrap() = "MERGED".into();
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Landed);
        assert_eq!(outcomes[0].landed_head.as_deref(), Some("c".repeat(40).as_str()));
        assert_eq!(runner.rebase_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 0);
    }

    /// Release-train PRs are outside this queue's contract: the gate rebases
    /// onto and compares `main`, so a PR targeting another branch is refused
    /// before tests rather than merged against an untested base.
    #[test]
    fn helm_merge_queue_rejects_a_non_main_pull_request_base() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let runner = FakeMergeRunner::new(&["changelog.d/crew-a-change.md"]);
        *runner.base.lock().unwrap() = "release/1".into();
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Failed);
        assert!(outcomes[0].reason.contains("targeting main"), "{}", outcomes[0].reason);
        assert_eq!(runner.rebase_calls.load(Ordering::SeqCst), 0);
    }

    /// The fragment convention is enforced before any rebase or gate starts:
    /// a branch that still stamps VERSION is refused even when it also has a
    /// fragment, because the stamp is exactly what makes parallel crew PRs
    /// collide.
    #[test]
    fn helm_merge_queue_rejects_stamped_version_files() {
        use std::sync::atomic::Ordering;
        let drive = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        ready_receipt(drive.path(), "crew-a", repo.path());
        let runner = FakeMergeRunner::new(&["VERSION", "changelog.d/crew-a-change.md"]);
        let outcomes = process_ready_receipts_with(drive.path(), &runner, None);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, MergeOutcomeStatus::Failed);
        assert!(outcomes[0].reason.contains("must not stamp VERSION"), "{}", outcomes[0].reason);
        assert_eq!(runner.rebase_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runner.land_calls.load(Ordering::SeqCst), 0);
    }

    // ── SUR-205: helm_digest_, the deterministic factual sections ─────────

    /// A drive seeded with `digest` as its DIGEST.md, in a tempdir that
    /// outlives the test.
    fn digest_drive(digest: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let drive = dir.path().join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join("DIGEST.md"), digest).unwrap();
        (dir, drive)
    }

    /// A report file as `crew_reports::write_report` writes it.
    fn seed_report(drive: &Path, chat_id: &str, word: &str, at: &str, body: &str) {
        std::fs::create_dir_all(drive.join("reports")).unwrap();
        std::fs::write(
            drive.join("reports").join(format!("{chat_id}.md")),
            format!("# Crew report: {chat_id}\n- Status: {word}\n- At: {at}\n\n{body}\n"),
        )
        .unwrap();
    }

    fn digest_crew(chat_id: &str, title: &str, live: bool) -> DigestCrew {
        DigestCrew {
            chat_id: chat_id.to_string(),
            title: title.to_string(),
            live,
        }
    }

    /// The lines under one `##` heading of a digest, for assertions that do
    /// not care about the order the bullets are written in.
    fn section_lines<'a>(digest: &'a str, heading: &str) -> Vec<&'a str> {
        let mut lines = Vec::new();
        let mut inside = false;
        for line in digest.lines() {
            if line.starts_with("## ") {
                if inside {
                    break;
                }
                inside = line.trim_end() == heading;
                continue;
            }
            if line.starts_with(REFRESHED) {
                break;
            }
            if inside && !line.trim().is_empty() {
                lines.push(line);
            }
        }
        lines
    }

    #[test]
    fn helm_digest_the_factual_sections_carry_what_the_reports_say() {
        let (_dir, drive) = digest_drive("## Needs you\n- approve the deploy\n");
        seed_report(
            &drive,
            "crew-b",
            "completed",
            "2026-11-25T10:00:00Z",
            "Invoices reconciled.",
        );
        let at = DateTime::parse_from_rfc3339("2026-11-25T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        refresh_digest(&drive, &[digest_crew("crew-b", "Billing", true)], at).unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert_eq!(
            section_lines(&digest, "## Needs you"),
            vec!["- approve the deploy"],
            "the model's Needs you survives: {digest}"
        );
        assert_eq!(
            section_lines(&digest, "## In progress"),
            vec!["- Billing (crew-b): completed at 2026-11-25T10:00:00Z — Invoices reconciled."],
            "the crew's report facts: {digest}"
        );
        assert_eq!(
            section_lines(&digest, "## New"),
            vec!["- Billing (crew-b): completed at 2026-11-25T10:00:00Z — Invoices reconciled."],
            "a first refresh has no cutoff, so the report is new: {digest}"
        );
        assert!(
            section_lines(&digest, "## Stalls").is_empty(),
            "a completed report is no stall: {digest}"
        );
        assert!(
            digest.contains("<!-- refreshed 2026-11-25T11:00:00Z -->"),
            "the ISO freshness timestamp: {digest}"
        );
    }

    #[test]
    fn helm_digest_archived_and_released_rows_disappear_immediately() {
        let (_dir, drive) = digest_drive("## Needs you\n- keep\n");
        seed_report(&drive, "crew-b", "completed", "2026-11-25T10:00:00Z", "done");
        seed_report(&drive, "crew-c", "completed", "2026-11-25T10:00:00Z", "done");
        let at = DateTime::parse_from_rfc3339("2026-11-25T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // First pass: both live, both listed.
        refresh_digest(
            &drive,
            &[digest_crew("crew-b", "Billing", true), digest_crew("crew-c", "Support", true)],
            at,
        )
        .unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert_eq!(section_lines(&digest, "## In progress").len(), 2, "{digest}");

        // crew-b's row is archived (live: false) and crew-c left the roster
        // entirely — the next refresh mentions neither.
        refresh_digest(&drive, &[digest_crew("crew-b", "Billing", false)], at).unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert!(
            !digest.contains("Billing") && !digest.contains("Support"),
            "archived and released rows leave every section: {digest}"
        );
        assert!(
            section_lines(&digest, "## In progress").is_empty()
                && section_lines(&digest, "## New").is_empty(),
            "no factual bullets survive: {digest}"
        );
    }

    #[test]
    fn helm_digest_needs_you_prose_is_preserved_intact_across_refreshes() {
        let prose = "## Needs you\nA1. approve the deploy — do it\n- pick a name\n";
        let (_dir, drive) = digest_drive(prose);
        let at = DateTime::parse_from_rfc3339("2026-11-25T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for _ in 0..3 {
            refresh_digest(&drive, &[], at).unwrap();
        }
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert_eq!(
            section_lines(&digest, "## Needs you"),
            vec!["A1. approve the deploy — do it", "- pick a name"],
            "lettered prose survives every refresh verbatim: {digest}"
        );
    }

    #[test]
    fn helm_digest_stalls_hold_the_error_and_aborted_reports() {
        let (_dir, drive) = digest_drive("## Needs you\n");
        seed_report(&drive, "crew-b", "error", "2026-11-25T10:00:00Z", "the build is red");
        seed_report(&drive, "crew-c", "aborted", "2026-11-25T10:00:00Z", "stopped halfway");
        seed_report(&drive, "crew-d", "completed", "2026-11-25T10:00:00Z", "fine");
        let at = DateTime::parse_from_rfc3339("2026-11-25T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        refresh_digest(
            &drive,
            &[
                digest_crew("crew-b", "One", true),
                digest_crew("crew-c", "Two", true),
                digest_crew("crew-d", "Three", true),
            ],
            at,
        )
        .unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        let stalls = section_lines(&digest, "## Stalls");
        assert_eq!(stalls.len(), 2, "{digest}");
        assert!(stalls.iter().any(|line| line.contains("- One (")), "{digest}");
        assert!(stalls.iter().any(|line| line.contains("- Two (")), "{digest}");
        assert_eq!(
            section_lines(&digest, "## In progress"),
            vec!["- Three (crew-d): completed at 2026-11-25T10:00:00Z — fine"],
            "a stalled crew leaves In progress: {digest}"
        );
    }

    #[test]
    fn helm_digest_new_drains_once_the_refresh_that_listed_it_is_written() {
        let (_dir, drive) = digest_drive("## Needs you\n");
        seed_report(&drive, "crew-b", "completed", "2026-11-25T10:00:00Z", "done");
        let first = DateTime::parse_from_rfc3339("2026-11-25T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let crew = [digest_crew("crew-b", "Billing", true)];
        refresh_digest(&drive, &crew, first).unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert_eq!(section_lines(&digest, "## New").len(), 1, "{digest}");

        // The next refresh, after the filing: the report predates the
        // previous synthesis, so New has drained.
        let later = DateTime::parse_from_rfc3339("2026-11-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        refresh_digest(&drive, &crew, later).unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert!(
            section_lines(&digest, "## New").is_empty(),
            "a report filed before the last refresh is no longer new: {digest}"
        );
        assert!(
            digest.contains("<!-- refreshed 2026-11-25T12:00:00Z -->"),
            "the freshness timestamp moved: {digest}"
        );
        assert_eq!(
            section_lines(&digest, "## In progress").len(),
            1,
            "the crew itself stays listed: {digest}"
        );
    }

    #[test]
    fn helm_digest_a_capitalized_needs_you_heading_survives() {
        // `parse_needs_you` reads the heading case-insensitively, so the
        // refresh must preserve what the snapshot would recognize.
        let (_dir, drive) = digest_drive("## Needs You\n- approve the deploy\n");
        let at = DateTime::parse_from_rfc3339("2026-11-25T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        refresh_digest(&drive, &[], at).unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert_eq!(
            section_lines(&digest, "## Needs You"),
            vec!["- approve the deploy"],
            "the asks survive under the model's own capitalization: {digest}"
        );
    }

    #[test]
    fn helm_digest_a_rewrite_dropping_the_marker_does_not_resurrect_reports() {
        // The model writes DIGEST.md whole; a write that drops the freshness
        // comment must not make every report new again — the cutoff is the
        // engine-owned sidecar's, not the file the model can overwrite.
        let (_dir, drive) = digest_drive("## Needs you\n");
        seed_report(&drive, "crew-b", "completed", "2026-11-25T10:00:00Z", "done");
        let first = DateTime::parse_from_rfc3339("2026-11-25T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let crew = [digest_crew("crew-b", "Billing", true)];
        refresh_digest(&drive, &crew, first).unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert_eq!(section_lines(&digest, "## New").len(), 1, "{digest}");

        std::fs::write(drive.join("DIGEST.md"), "## Needs you\n- half-written\n").unwrap();
        let later = DateTime::parse_from_rfc3339("2026-11-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        refresh_digest(&drive, &crew, later).unwrap();
        let digest = std::fs::read_to_string(drive.join("DIGEST.md")).unwrap();
        assert!(
            section_lines(&digest, "## New").is_empty(),
            "the sidecar's cutoff survives the rewrite: {digest}"
        );
        assert_eq!(
            section_lines(&digest, "## Needs you"),
            vec!["- half-written"],
            "and the model's edit is the section the refresh keeps: {digest}"
        );
    }

    #[test]
    fn helm_digest_the_write_is_atomic_under_concurrent_refreshes() {
        let (_dir, drive) = digest_drive("## Needs you\n- keep\n");
        seed_report(&drive, "crew-b", "completed", "2026-11-25T10:00:00Z", "done");
        let drive = Arc::new(drive);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut writers = Vec::new();
        for index in 0..4 {
            let drive = drive.clone();
            writers.push(std::thread::spawn(move || {
                for round in 0..50 {
                    let at = Utc::now();
                    refresh_digest(&drive, &[digest_crew("crew-b", "Billing", true)], at)
                        .unwrap_or_else(|error| panic!("writer {index} round {round}: {error}"));
                }
            }));
        }
        let reader_drive = drive.clone();
        let reader_stop = stop.clone();
        let reader = std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            while !reader_stop.load(Ordering::Relaxed) {
                if let Ok(digest) = std::fs::read_to_string(reader_drive.join("DIGEST.md"))
                    && !digest.is_empty()
                    && digest != "## Needs you\n- keep\n"
                {
                    // Anything but the pristine seed is a synthesized digest,
                    // and it is complete or the write was not atomic.
                    assert!(
                        digest.contains("## Needs you")
                            && digest.contains("## New")
                            && digest.contains("## Stalls")
                            && digest.contains("<!-- refreshed "),
                        "every read is a complete digest: {}",
                        &digest[..digest.len().min(200)]
                    );
                }
            }
        });
        for writer in writers {
            writer.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();
        // No temp file outlives the renames.
        let leftovers: Vec<_> = std::fs::read_dir(drive.as_ref())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp files linger: {leftovers:?}");
    }
}
