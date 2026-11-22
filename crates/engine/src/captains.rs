//! `captains.json` is engine-owned state recording which chats are Captains
//! and who is in each crew. The engine is the only writer; the dsh-captains
//! plugin never reads it (it gets its role stamped per run). The Drive
//! directory is the plugin's to create — this module only computes its path.

// Nothing calls in yet: this commit lands the API and its tests only.
#![allow(dead_code)]

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

/// `<deepseek_home>/captains/<captain_id>` — where a Captain's Drive
/// directory belongs. Pure: computes the path; the plugin creates it. The id
/// is used as given: validating a request id against the live registry
/// belongs at the RPC boundary (SUR-82) — this module's ids are engine-issued
/// chat ids.
pub fn drive_dir(deepseek_home: &Path, captain_id: &str) -> PathBuf {
    deepseek_home.join("captains").join(captain_id)
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
}

impl CaptainsStore {
    /// Open the store over `data_dir`, loading `captains.json` once. A
    /// malformed file is `Err`; nothing is published.
    pub fn open(data_dir: PathBuf) -> Result<CaptainsStore, String> {
        let snapshot = Registry::load(&data_dir)?;
        Ok(CaptainsStore {
            data_dir,
            snapshot: Mutex::new(snapshot),
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

    /// One read/modify/write round: take the process-wide lock, re-read the
    /// file fresh under it, apply the rules, save atomically, and only then
    /// publish.
    fn mutate(
        &self,
        change: impl FnOnce(&mut Registry) -> Result<(), String>,
    ) -> Result<(), String> {
        #[cfg(test)]
        test_seam::arrive();
        let _guard = MUTATION.lock().unwrap_or_else(PoisonError::into_inner);
        let mut fresh = Registry::load(&self.data_dir)?;
        change(&mut fresh)?;
        fresh.save(&self.data_dir)?;
        *self
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = fresh;
        Ok(())
    }

    /// Promote `chat_id` to Captain under `name` (see `Registry::promote`).
    pub fn promote(&self, chat_id: &str, name: &str) -> Result<(), String> {
        self.mutate(|registry| registry.promote(chat_id, name))
    }

    /// Demote `chat_id`'s Captain (see `Registry::demote`).
    pub fn demote(&self, chat_id: &str) -> Result<(), String> {
        self.mutate(|registry| registry.demote(chat_id))
    }

    /// Add `chat_id` to `captain_id`'s crew (see `Registry::add_crew`).
    pub fn add_crew(&self, captain_id: &str, chat_id: &str) -> Result<(), String> {
        self.mutate(|registry| registry.add_crew(captain_id, chat_id))
    }

    /// Remove `chat_id` from `captain_id`'s crew (see
    /// `Registry::remove_crew`).
    pub fn remove_crew(&self, captain_id: &str, chat_id: &str) -> Result<(), String> {
        self.mutate(|registry| registry.remove_crew(captain_id, chat_id))
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
}
