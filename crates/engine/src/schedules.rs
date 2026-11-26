//! Engine-owned scheduled tasks: dsh's schedule service is a long-lived Host
//! service, but zeron's DeepSeek sidecar is per-run, so the engine keeps the
//! task store (`{data_dir}/schedules.json`, atomic temp+rename writes) and
//! fires each occurrence by queueing its prompt on a chat — the same internal
//! row `QueueMessage` appends.
//!
//! A fired occurrence remembers its queued row (`inFlight`). While that row
//! still waits in the target chat's queue, or is the turn the chat is running,
//! a later occurrence of the same task on the same chat is coalesced into it
//! (consumed, not queued again): a busy Captain gets one wake, not a backlog
//! of copies. The marker is only ever checked against the chat's live state,
//! and expires after [`IN_FLIGHT_MAX_MS`], so a crash cannot leave it
//! suppressing later occurrences.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeron_rpc::{RpcError, methods};

use crate::{EngineError, new_id, now_ms};

/// One stored task. `id` empty on save means create; times are epoch ms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    #[serde(default)]
    pub id: String,
    /// At most 120 chars.
    pub title: String,
    pub prompt: String,
    /// `DTSTART[;TZID=Zone]:YYYYMMDDTHHMMSS[Z]` + `RRULE:...`, two lines.
    pub rrule: String,
    pub target: Target,
    #[serde(default = "on")]
    pub active: bool,
    #[serde(default)]
    pub next_at: Option<i64>,
    #[serde(default)]
    pub last_run_at: Option<i64>,
    #[serde(default)]
    pub updated_at: i64,
    /// The last delivery's queued row; engine-owned (a save keeps the
    /// stored one). Absent in stores written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<InFlight>,
}

/// One delivered occurrence's queued row on its chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InFlight {
    pub session: String,
    pub row: String,
    /// Epoch ms the row was queued.
    pub queued_at: i64,
}

fn on() -> bool {
    true
}

/// Where a fired occurrence's prompt goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    /// Every occurrence queues on this one chat.
    Session { session: String },
    /// The first occurrence creates one chat (in `cwd`); later ones go to it.
    #[serde(rename = "new_once")]
    NewOnce {
        #[serde(default)]
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Every occurrence creates a fresh chat in `cwd`.
    New {
        #[serde(default)]
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
}

// ── rrule subset ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Freq {
    Minutely,
    Hourly,
    Daily,
    Weekly,
}

/// dsh's rrule subset: one `DTSTART` line + one `RRULE` line with FREQ of
/// MINUTELY|HOURLY|DAILY|WEEKLY plus INTERVAL, BYDAY, BYHOUR, BYMINUTE, COUNT
/// and UNTIL. No chrono-tz in the tree, so `TZID` is UTC (or an alias) or a
/// fixed `±HH:MM` offset — a named zone is refused, not guessed. All period
/// math is wall time in that fixed offset, so no DST case exists.
struct Rule {
    zone: FixedOffset,
    /// DTSTART's wall time in `zone`; always the first occurrence (RFC 5545).
    start: NaiveDateTime,
    until: Option<NaiveDateTime>,
    freq: Freq,
    interval: i64,
    days: Option<Vec<Weekday>>,
    hours: Option<Vec<u32>>,
    minutes: Option<Vec<u32>>,
    count: Option<u64>,
}

fn parse_rrule(src: &str) -> Result<Rule, String> {
    let mut lines = src.lines().map(str::trim).filter(|line| !line.is_empty());
    let (Some(dtstart), Some(rrule), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err("rrule is a DTSTART line and an RRULE line".into());
    };
    // The last colon: `TZID=+05:30` carries one itself.
    let (head, value) = dtstart.rsplit_once(':').ok_or("DTSTART has no value")?;
    let zone = parse_zone(
        head.strip_prefix("DTSTART")
            .ok_or("first line must be DTSTART")?,
        value.ends_with('Z'),
    )?;
    let mut rule = Rule {
        zone,
        start: naive(value.strip_suffix('Z').unwrap_or(value), "DTSTART")?,
        until: None,
        freq: Freq::Daily,
        interval: 1,
        days: None,
        hours: None,
        minutes: None,
        count: None,
    };
    let mut freq = None;
    for part in rrule
        .strip_prefix("RRULE:")
        .ok_or("second line must be RRULE:")?
        .split(';')
    {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("bad RRULE part {part:?}"))?;
        match key {
            "FREQ" if freq.is_none() => {
                freq = Some(match value {
                    "MINUTELY" => Freq::Minutely,
                    "HOURLY" => Freq::Hourly,
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    _ => return Err(format!("unsupported FREQ={value}")),
                });
            }
            "INTERVAL" => {
                rule.interval = value
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or("INTERVAL must be a positive integer")?
            }
            "BYDAY" => rule.days = Some(value.split(',').map(day).collect::<Result<_, _>>()?),
            "BYHOUR" => rule.hours = Some(nums(value, 24, "BYHOUR")?),
            "BYMINUTE" => rule.minutes = Some(nums(value, 60, "BYMINUTE")?),
            "COUNT" => {
                rule.count = Some(
                    value
                        .parse::<u64>()
                        .ok()
                        .filter(|n| *n >= 1)
                        .ok_or("COUNT must be a positive integer")?,
                )
            }
            // A Z-marked UNTIL is an instant: re-express it in the rule's
            // offset (already UTC's when DTSTART is Z or TZID-less).
            "UNTIL" => {
                rule.until = Some(match value.strip_suffix('Z') {
                    Some(v) => {
                        naive(v, "UNTIL")?
                            + chrono::Duration::seconds(zone.local_minus_utc() as i64)
                    }
                    None => naive(value, "UNTIL")?,
                });
            }
            other => return Err(format!("unsupported RRULE part {other}")),
        }
    }
    rule.freq = freq.ok_or("RRULE needs FREQ")?;
    Ok(rule)
}

/// `DTSTART`'s params (`;TZID=...` or empty) plus whether the value ends `Z`.
fn parse_zone(params: &str, zulu: bool) -> Result<FixedOffset, String> {
    let utc = FixedOffset::east_opt(0).unwrap();
    let named = match params {
        "" => None,
        _ => Some(
            match params.strip_prefix(";TZID=").ok_or("bad DTSTART params")? {
                "UTC" | "GMT" | "Z" | "Etc/UTC" | "Etc/GMT" | "Etc/Universal" => utc,
                id => {
                    let (sign, digits) = match id.as_bytes().first() {
                        Some(b'+') => (1i32, &id[1..]),
                        Some(b'-') => (-1i32, &id[1..]),
                        _ => {
                            return Err(format!(
                                "TZID {id:?}: named zones need chrono-tz, which zeron doesn't carry — use UTC or a fixed offset like +05:30"
                            ));
                        }
                    };
                    let (h, m) = match digits.split_once(':') {
                        Some(pair) => pair,
                        None if digits.len() == 4 => digits.split_at(2),
                        None => (digits, "0"),
                    };
                    let num = |s: &str| s.parse::<i32>().ok().filter(|n| *n >= 0);
                    let secs = sign
                        * (num(h).filter(|h| *h < 24).ok_or("bad TZID offset")? * 3600
                            + num(m).filter(|m| *m < 60).ok_or("bad TZID offset")? * 60);
                    FixedOffset::east_opt(secs).ok_or("TZID offset out of range")?
                }
            },
        ),
    };
    match (named, zulu) {
        (Some(zone), true) if zone == utc => Ok(utc),
        (Some(_), true) => Err("DTSTART: 'Z' with a non-UTC TZID".into()),
        (Some(zone), false) => Ok(zone),
        (None, _) => Ok(utc),
    }
}

fn naive(value: &str, what: &str) -> Result<NaiveDateTime, String> {
    NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .map_err(|_| format!("{what} must be YYYYMMDDTHHMMSS"))
}

fn day(name: &str) -> Result<Weekday, String> {
    Ok(match name {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return Err(format!("BYDAY {name:?}: only bare weekdays (MO..SU)")),
    })
}

fn nums(value: &str, bound: u32, part: &str) -> Result<Vec<u32>, String> {
    value
        .split(',')
        .map(|n| {
            n.parse::<u32>()
                .ok()
                .filter(|n| *n < bound)
                .ok_or_else(|| format!("{part} values are under {bound}"))
        })
        .collect()
}

impl Rule {
    /// Wall-time start of the period covering `start`: its minute, hour, day
    /// or Monday-dated week. Period `n` opens `n * interval` units later.
    fn base(&self) -> NaiveDateTime {
        let s = self.start;
        match self.freq {
            Freq::Minutely => s.with_second(0).unwrap(),
            Freq::Hourly => s.with_minute(0).unwrap().with_second(0).unwrap(),
            Freq::Daily => s.date().and_time(NaiveTime::MIN),
            Freq::Weekly => {
                s.date().and_time(NaiveTime::MIN)
                    - chrono::Duration::days(s.weekday().num_days_from_monday() as i64)
            }
        }
    }

    /// The period index containing `after` (0 when `after` precedes the
    /// first): everything earlier periods yield precedes `after`, so a rule
    /// with no COUNT to tally may start its scan there.
    fn period_of(&self, after: NaiveDateTime) -> u64 {
        let span = match self.freq {
            Freq::Minutely => 60,
            Freq::Hourly => 3_600,
            Freq::Daily => 86_400,
            Freq::Weekly => 604_800,
        };
        ((after - self.base()).num_seconds().max(0) / span / self.interval) as u64
    }

    /// A limit check (BYxxx at or above FREQ's unit filters; below expands).
    fn day_ok(&self, t: NaiveDateTime) -> bool {
        self.days.as_ref().is_none_or(|v| v.contains(&t.weekday()))
    }
    fn hour_ok(&self, t: NaiveDateTime) -> bool {
        self.hours.as_ref().is_none_or(|v| v.contains(&t.hour()))
    }
    fn min_ok(&self, t: NaiveDateTime) -> bool {
        self.minutes
            .as_ref()
            .is_none_or(|v| v.contains(&t.minute()))
    }

    /// The occurrences inside period `n` (wall time), sorted, none before
    /// `start`.
    fn expand(&self, n: u64) -> Vec<NaiveDateTime> {
        use chrono::Duration as Cd;
        let step = self.interval * n as i64;
        let days: Vec<chrono::NaiveDate> = match self.freq {
            Freq::Minutely => {
                let t = self.base() + Cd::minutes(step) + Cd::seconds(self.start.second() as i64);
                return if t >= self.start && self.day_ok(t) && self.hour_ok(t) && self.min_ok(t) {
                    vec![t]
                } else {
                    vec![]
                };
            }
            Freq::Hourly => {
                let h = self.base() + Cd::hours(step);
                return if self.day_ok(h) && self.hour_ok(h) {
                    self.clock_times(h.date(), Some(h.hour()))
                } else {
                    vec![]
                };
            }
            Freq::Daily => {
                let d = self.base() + Cd::days(step);
                if !self.day_ok(d) {
                    return vec![];
                }
                vec![d.date()]
            }
            Freq::Weekly => self
                .days
                .clone()
                .unwrap_or_else(|| vec![self.start.weekday()])
                .into_iter()
                .map(|d| {
                    self.base().date() + Cd::weeks(step) + Cd::days(d.num_days_from_monday() as i64)
                })
                .collect(),
        };
        days.into_iter()
            .flat_map(|d| self.clock_times(d, None))
            .collect()
    }

    /// The BYHOUR × BYMINUTE times on `date` at `start`'s second — `hour`
    /// pins BYHOUR for the hourly form — each at or after `start`.
    fn clock_times(&self, date: chrono::NaiveDate, hour: Option<u32>) -> Vec<NaiveDateTime> {
        let hours = hour.map_or_else(
            || {
                self.hours
                    .clone()
                    .unwrap_or_else(|| vec![self.start.hour()])
            },
            |h| vec![h],
        );
        let minutes = self
            .minutes
            .clone()
            .unwrap_or_else(|| vec![self.start.minute()]);
        let mut out: Vec<_> = hours
            .iter()
            .flat_map(|h| minutes.iter().map(move |m| (*h, *m)))
            .filter_map(|(h, m)| NaiveTime::from_hms_opt(h, m, self.start.second()))
            .map(|t| date.and_time(t))
            .filter(|t| *t >= self.start)
            .collect();
        out.sort();
        out
    }

    /// The first occurrence strictly after `after`, or none once the rule is
    /// spent (COUNT) or over (UNTIL). Missed occurrences collapse into the
    /// next one — never a backlog replay.
    fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let after = after.with_timezone(&self.zone).naive_local();
        let mut seen = 0u64;
        let mut last = self.start - chrono::Duration::nanoseconds(1);
        // DTSTART is the first occurrence whether or not the rule emits it.
        let mut pending = vec![self.start];
        let mut period = match self.count {
            Some(_) => 0, // COUNT tallies every occurrence: walk from DTSTART.
            None => self.period_of(after),
        };
        while period <= 4_000_000 {
            if pending.is_empty() {
                pending = self.expand(period);
                period += 1;
                continue;
            }
            let t = pending.remove(0);
            if t == last {
                continue;
            }
            last = t;
            if self.until.is_some_and(|u| t > u) {
                return None;
            }
            seen += 1;
            if self.count.is_some_and(|c| seen > c) {
                return None;
            }
            if t > after {
                return Some(self.utc(t));
            }
        }
        None
    }

    fn utc(&self, t: NaiveDateTime) -> DateTime<Utc> {
        self.zone
            .from_local_datetime(&t)
            .single()
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|| DateTime::from_naive_utc_and_offset(t, Utc))
    }
}

// ── store, delivery and the runner ──────────────────────────────────────────

/// What one fired occurrence did. A lifecycle outcome is not a transport
/// error: asleep Captains skip the occurrence, demoted Captains end the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The prompt landed on `session` as queued row `row`.
    Delivered { session: String, row: String },
    /// The target is a user-archived Captain: asleep, not dead.
    SkippedAsleep,
    /// The target is a demoted Captain: no live Captain remains to wake.
    Ended,
}

/// One fired occurrence's delivery; the engine's [`SessionDeliver`] is what
/// runs in production, tests substitute a recorder.
pub trait Deliver: Send + Sync {
    fn deliver(&self, task: &Task) -> Result<Delivery, EngineError>;
    /// Whether queued row `row` still owes `session` its turn (queued, or
    /// the turn running now).
    fn pending(&self, session: &str, row: &str) -> Result<bool, EngineError>;
}

/// Queues a fired prompt on the target chat's doc — the row `QueueMessage`
/// appends — creating `new_once`/`new` chats first (`cwd`, this device, the
/// default harness: `Mutate`'s `CreateChat` without a space or a parent).
pub struct SessionDeliver {
    pub doc_host: crate::doc_host::DocHost,
    pub workspace: crate::workspace_host::WorkspaceHost,
    /// Captain roster location, read at fire time so archive/demote lands
    /// without waiting for the engine process to restart.
    pub data_dir: PathBuf,
}

impl SessionDeliver {
    /// The Captain lifecycle for a direct `target.kind = "session"` delivery.
    /// `queue_message` unarchives ordinary chats on send, so this decision has
    /// to happen before that call: a hidden Captain is asleep and a demoted
    /// Captain is no longer a Captain at all.
    fn captain_outcome(&self, session: &str) -> Result<Option<Delivery>, EngineError> {
        let registry =
            crate::captains::Registry::load(&self.data_dir).map_err(EngineError::Other)?;
        let Some(captain) = registry
            .captains()
            .iter()
            .find(|captain| captain.chat_id == session)
        else {
            return Ok(None);
        };
        if captain.archived {
            return Ok(Some(Delivery::Ended));
        }
        if captain.user_archived {
            return Ok(Some(Delivery::SkippedAsleep));
        }
        Ok(None)
    }
}

impl Deliver for SessionDeliver {
    fn deliver(&self, task: &Task) -> Result<Delivery, EngineError> {
        let session = match &task.target {
            Target::Session { session } => {
                if let Some(outcome) = self.captain_outcome(session)? {
                    return Ok(outcome);
                }
                session.clone()
            }
            Target::NewOnce {
                session: Some(session),
                ..
            } => {
                if let Some(outcome) = self.captain_outcome(session)? {
                    return Ok(outcome);
                }
                session.clone()
            }
            Target::NewOnce { cwd, .. } | Target::New { cwd, .. } => {
                let session = new_id();
                self.workspace.create_chat(
                    &session,
                    None,
                    Some(self.doc_host.device_id()),
                    None,
                    Some(cwd.clone()),
                )?;
                session
            }
        };
        let row = self
            .doc_host
            .queue_message(&session, &task.prompt, Vec::new())?;
        Ok(Delivery::Delivered { session, row })
    }

    /// A hidden or demoted Captain owes no wake: the occurrence goes to
    /// `deliver`, which skips or ends the task for it.
    fn pending(&self, session: &str, row: &str) -> Result<bool, EngineError> {
        if self.captain_outcome(session)?.is_some() {
            return Ok(false);
        }
        self.doc_host.queued_row_pending(session, row)
    }
}

/// The task store and runner. Cheap to clone (one `Arc` inside).
#[derive(Clone)]
pub struct Schedules {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    tasks: Mutex<Vec<Task>>,
    deliver: Arc<dyn Deliver>,
    /// Wakes the loop early when the store changes under it.
    kick: Arc<tokio::sync::Notify>,
}

/// dsh's delivery floor: at least a minute between one task's runs. Also the
/// runner's longest sleep, so a failed delivery retries within it.
const MIN_RUN_GAP_MS: i64 = 60_000;
const POLL_CAP: Duration = Duration::from_secs(60);
/// An in-flight marker older than this no longer coalesces, whatever the
/// chat says: a row stuck in a paused queue costs one extra wake a day at
/// most, never the schedule.
const IN_FLIGHT_MAX_MS: i64 = 24 * 60 * 60 * 1000;

impl Schedules {
    /// Open `{data_dir}/schedules.json` (written on first change).
    pub fn open(data_dir: &Path, deliver: Arc<dyn Deliver>) -> Self {
        let path = data_dir.join("schedules.json");
        let tasks = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| {
                serde_json::from_str::<Vec<Task>>(&raw)
                    .map_err(
                        |error| tracing::warn!(%error, "schedules: store unread, starting empty"),
                    )
                    .ok()
            })
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                path,
                tasks: Mutex::new(tasks),
                deliver,
                kick: Arc::new(tokio::sync::Notify::new()),
            }),
        }
    }

    /// The wake loop: sleep to the earliest `nextAt` (never longer than
    /// [`POLL_CAP`]), fire what's due, repeat. No runtime, no runner — the
    /// store still answers RPCs.
    ///
    /// The loop holds the store weakly, as `plugins`' worker does: a strong
    /// handle here would keep the engine alive after its window closed, so
    /// the engine would never retire.
    pub fn start(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = Arc::downgrade(&self.inner);
        runtime.spawn(async move {
            loop {
                let (wait, kick) = {
                    // Upgraded for one tick only: dropped before the sleep, so
                    // an idle runner holds nothing.
                    let Some(inner) = weak.upgrade() else { return };
                    let this = Schedules { inner };
                    (this.tick(Utc::now()), this.inner.kick.clone())
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait.min(POLL_CAP)) => {}
                    _ = kick.notified() => {}
                }
            }
        });
    }

    /// Fire each task due at `now` once — however many occurrences passed
    /// while down, only the latest missed one runs — then recompute its
    /// `nextAt` from `now`. Returns the wait to the earliest due; the clock
    /// is a parameter so tests drive the runner without sessions or time.
    fn tick(&self, now: DateTime<Utc>) -> Duration {
        let now_ms = now.timestamp_millis();
        let mut wait = POLL_CAP;
        let mut tasks = self.tasks();
        let mut dirty = false;
        for task in tasks.iter_mut() {
            if !task.active {
                continue;
            }
            let Some(next) = task.next_at else { continue };
            if next > now_ms {
                wait = wait.min(to_dur(next - now_ms));
                continue;
            }
            if let Some(last) = task
                .last_run_at
                .filter(|last| now_ms - last < MIN_RUN_GAP_MS)
            {
                wait = wait.min(to_dur(last + MIN_RUN_GAP_MS - now_ms));
                continue;
            }
            if task.in_flight.is_some() {
                dirty = true;
                if self.coalesces(task, now_ms) {
                    // The previous wake has not been taken up yet: this
                    // occurrence folds into it. Nothing new ran, so lastRunAt
                    // stays; the marker stays until that delivery finishes.
                    tracing::info!(schedule = %task.id, "scheduled wake coalesced into the pending one");
                    task.updated_at = now_ms;
                    advance(task, now, now_ms, &mut wait);
                    continue;
                }
                task.in_flight = None;
            }
            match self.inner.deliver.deliver(task) {
                Ok(Delivery::Delivered { session, row }) => {
                    dirty = true;
                    if let Target::NewOnce { session: at, .. } | Target::New { session: at, .. } =
                        &mut task.target
                    {
                        *at = Some(session.clone());
                    }
                    task.in_flight = Some(InFlight {
                        session,
                        row,
                        queued_at: now_ms,
                    });
                    task.last_run_at = Some(now_ms);
                    task.updated_at = now_ms;
                    advance(task, now, now_ms, &mut wait);
                }
                // A hidden Captain is asleep, not failed: consume this
                // occurrence without touching lastRunAt and move to the next
                // future one. `queue_message` never runs, so the archived row
                // is not revived.
                Ok(Delivery::SkippedAsleep) => {
                    dirty = true;
                    task.updated_at = now_ms;
                    advance(task, now, now_ms, &mut wait);
                }
                // A demoted Captain is not a valid target for a later turn
                // either. Ending the task is durable; a transport error, by
                // contrast, keeps `nextAt` and retries within POLL_CAP.
                Ok(Delivery::Ended) => {
                    dirty = true;
                    task.active = false;
                    task.next_at = None;
                    task.updated_at = now_ms;
                }
                Err(error) => {
                    tracing::warn!(schedule = %task.id, %error, "scheduled delivery failed")
                }
            }
        }
        if dirty {
            self.persist(&tasks);
        }
        wait
    }

    /// Whether this occurrence folds into the task's in-flight delivery:
    /// same chat as the target (a `new` target's every occurrence is a
    /// fresh chat, so never), marker younger than [`IN_FLIGHT_MAX_MS`], and
    /// the row still owed. A failed check delivers — it never suppresses.
    fn coalesces(&self, task: &Task, now_ms: i64) -> bool {
        let Some(flight) = &task.in_flight else {
            return false;
        };
        let target = match &task.target {
            Target::Session { session } => session,
            Target::NewOnce {
                session: Some(session),
                ..
            } => session,
            Target::NewOnce { session: None, .. } | Target::New { .. } => return false,
        };
        if *target != flight.session || now_ms - flight.queued_at >= IN_FLIGHT_MAX_MS {
            return false;
        }
        self.inner
            .deliver
            .pending(&flight.session, &flight.row)
            .unwrap_or_else(|error| {
                tracing::warn!(schedule = %task.id, %error, "schedules: in-flight check failed");
                false
            })
    }

    fn list(&self) -> Vec<Task> {
        self.tasks().clone()
    }

    /// ScheduleSave: create (`id` absent) or replace (`id` present). The
    /// rrule is validated and `nextAt` recomputed; a rule with no future
    /// occurrence is refused rather than saved dead.
    fn save(&self, params: &Value) -> Result<Task, RpcError> {
        let bad = RpcError::BadParams;
        let mut task: Task =
            serde_json::from_value(params.get("task").cloned().unwrap_or(Value::Null))
                .map_err(|error| bad(format!("bad task: {error}")))?;
        if task.title.chars().count() > 120 {
            return Err(bad("title is 120 chars at most".into()));
        }
        if task.prompt.trim().is_empty() {
            return Err(bad("prompt is empty".into()));
        }
        if matches!(&task.target, Target::Session { session } if session.is_empty()) {
            return Err(bad("target kind \"session\" needs a session".into()));
        }
        let rule = parse_rrule(&task.rrule).map_err(bad)?;
        task.next_at = match task.active {
            true => Some(
                rule.next_after(Utc::now())
                    .map(|t| t.timestamp_millis())
                    .ok_or_else(|| bad("rrule has no occurrence in the future".into()))?,
            ),
            false => None,
        };
        task.updated_at = now_ms();
        let mut tasks = self.tasks();
        let stored = tasks
            .iter()
            .find(|t| !task.id.is_empty() && t.id == task.id);
        task.last_run_at = stored.and_then(|t| t.last_run_at);
        task.in_flight = stored.and_then(|t| t.in_flight.clone());
        match tasks.iter_mut().find(|t| t.id == task.id) {
            Some(found) => *found = task.clone(),
            None => {
                task.id = new_id();
                tasks.push(task.clone());
            }
        }
        self.persist(&tasks);
        self.inner.kick.notify_one();
        Ok(task)
    }

    fn delete(&self, params: &Value) -> Result<Value, RpcError> {
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::BadParams("delete takes an id".into()))?;
        let mut tasks = self.tasks();
        let gone = tasks
            .iter()
            .position(|t| t.id == id)
            .map(|i| tasks.remove(i))
            .is_some();
        if gone {
            self.persist(&tasks);
            self.inner.kick.notify_one();
        }
        Ok(json!({ "deleted": gone }))
    }

    /// ScheduleRunNow: fire once now; `nextAt` is kept — the schedule still
    /// owes that occurrence. A manual run is an explicit ask, so it bypasses
    /// coalescing and always queues; its row becomes the task's in-flight
    /// delivery, so a scheduled occurrence due behind it folds into it.
    fn run_now(&self, params: &Value) -> Result<Value, RpcError> {
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::BadParams("runNow takes an id".into()))?;
        let mut tasks = self.tasks();
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| RpcError::Failed(format!("no such schedule: {id}")))?;
        let delivery = self
            .inner
            .deliver
            .deliver(task)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let Delivery::Delivered { session, row } = delivery else {
            return Err(RpcError::Failed(match delivery {
                Delivery::SkippedAsleep => "the Captain is archived and asleep".into(),
                Delivery::Ended => "the Captain has been demoted".into(),
                Delivery::Delivered { .. } => unreachable!(),
            }));
        };
        if let Target::NewOnce { session: at, .. } | Target::New { session: at, .. } =
            &mut task.target
        {
            *at = Some(session.clone());
        }
        let now = now_ms();
        task.in_flight = Some(InFlight {
            session: session.clone(),
            row,
            queued_at: now,
        });
        task.last_run_at = Some(now);
        task.updated_at = now;
        self.persist(&tasks);
        Ok(json!({ "fired": true, "session": session }))
    }

    fn tasks(&self) -> MutexGuard<'_, Vec<Task>> {
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The task list to `schedules.json.tmp`, renamed over the store: a
    /// reader never meets half a file.
    fn persist(&self, tasks: &[Task]) {
        if let Some(parent) = self.inner.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.inner.path.with_extension("tmp");
        let written = serde_json::to_string_pretty(tasks)
            .map_err(std::io::Error::other)
            .and_then(|raw| {
                std::fs::write(&tmp, raw).and_then(|()| std::fs::rename(&tmp, &self.inner.path))
            });
        if let Err(error) = written {
            tracing::warn!(%error, "schedules: persist failed");
        }
    }
}

/// The schedule_mcp view of a task's rule: its fixed UTC offset in seconds
/// (for rendering `scheduledAtLocal`) and its next occurrence after `now`
/// — `None` once the rule is spent, which is how a stored `active: false`
/// task reads as "ended" rather than "paused". `None` on a rule the store
/// would never have saved (defensive).
pub(crate) fn rule_view(rrule: &str, now: DateTime<Utc>) -> Option<(i64, Option<DateTime<Utc>>)> {
    let rule = parse_rrule(rrule).ok()?;
    Some((rule.zone.local_minus_utc() as i64, rule.next_after(now)))
}

/// Consume the occurrence due at `now`: `nextAt` moves to the next one (the
/// task ends when none is left) and `wait` shortens to it.
fn advance(task: &mut Task, now: DateTime<Utc>, now_ms: i64, wait: &mut Duration) {
    task.next_at = parse_rrule(&task.rrule)
        .ok()
        .and_then(|rule| rule.next_after(now))
        .map(|t| t.timestamp_millis());
    match task.next_at {
        Some(next) => *wait = (*wait).min(to_dur(next - now_ms)),
        None => task.active = false,
    }
}

fn to_dur(ms: i64) -> Duration {
    Duration::from_millis(ms.max(0) as u64)
}

/// Whether `method` is answered here.
pub fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::SCHEDULES
            | methods::SCHEDULE_SAVE
            | methods::SCHEDULE_DELETE
            | methods::SCHEDULE_RUN_NOW
    )
}

/// Answer one of the four methods.
pub fn call(schedules: &Schedules, method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        methods::SCHEDULES => Ok(json!({ "tasks": schedules.list() })),
        methods::SCHEDULE_SAVE => serde_json::to_value(schedules.save(params)?)
            .map_err(|e| RpcError::Failed(e.to_string())),
        methods::SCHEDULE_DELETE => schedules.delete(params),
        methods::SCHEDULE_RUN_NOW => schedules.run_now(params),
        other => Err(RpcError::UnknownMethod(other.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(at: &str) -> DateTime<Utc> {
        at.parse().unwrap()
    }

    /// The RFC 3339 second the rule fires next at, "" when spent.
    fn next(src: &str, after: &str) -> String {
        parse_rrule(src)
            .unwrap()
            .next_after(after.parse().unwrap())
            .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_default()
    }

    #[test]
    fn daily_at_nine_and_weekdays_by_byday() {
        let daily = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY";
        assert_eq!(next(daily, "2026-01-05T08:59:00Z"), "2026-01-05T09:00:00Z");
        assert_eq!(next(daily, "2026-01-05T09:00:00Z"), "2026-01-06T09:00:00Z");
        // 2026-01-05 is a Monday: MO,WE,FR at 09:00.
        let weekdays =
            "DTSTART:20260105T090000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR;BYHOUR=9;BYMINUTE=0";
        assert_eq!(
            next(weekdays, "2026-01-05T09:00:00Z"),
            "2026-01-07T09:00:00Z"
        );
        assert_eq!(
            next(weekdays, "2026-01-07T09:00:00Z"),
            "2026-01-09T09:00:00Z"
        );
        assert_eq!(
            next(weekdays, "2026-01-09T09:00:00Z"),
            "2026-01-12T09:00:00Z"
        );
        // A fixed-offset TZID: 09:00 at +05:30 lands 03:30Z; a named zone is refused.
        let offset = "DTSTART;TZID=+05:30:20260101T090000\nRRULE:FREQ=DAILY";
        assert_eq!(next(offset, "2026-01-04T10:00:00Z"), "2026-01-05T03:30:00Z");
        assert!(
            parse_rrule("DTSTART;TZID=America/New_York:20260101T090000\nRRULE:FREQ=DAILY").is_err()
        );
    }

    #[test]
    fn count_and_until_end_the_rule() {
        let count = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY;COUNT=3";
        assert_eq!(next(count, "2026-01-03T08:00:00Z"), "2026-01-03T09:00:00Z");
        assert_eq!(next(count, "2026-01-03T09:00:00Z"), "");
        let until = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY;UNTIL=20260103T090000Z";
        assert_eq!(next(until, "2026-01-02T10:00:00Z"), "2026-01-03T09:00:00Z");
        assert_eq!(next(until, "2026-01-03T09:00:00Z"), "");
    }

    /// Records deliveries as `id:prompt`; each queues row `row-N`, and the
    /// rows in `pending` are the ones a chat still owes a turn.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>, Mutex<Vec<String>>);
    impl Recorder {
        fn new() -> Self {
            Self::default()
        }
        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }
        /// The delivered row finished its turn: nothing pending remains.
        fn finish_all(&self) {
            self.1.lock().unwrap().clear();
        }
    }
    impl Deliver for Recorder {
        fn deliver(&self, task: &Task) -> Result<Delivery, EngineError> {
            let mut log = self.0.lock().unwrap();
            log.push(format!("{}:{}", task.id, task.prompt));
            let row = format!("row-{}", log.len());
            self.1.lock().unwrap().push(row.clone());
            let session = match &task.target {
                Target::Session { session } => session.clone(),
                _ => "chat-1".into(),
            };
            Ok(Delivery::Delivered { session, row })
        }
        fn pending(&self, _session: &str, row: &str) -> Result<bool, EngineError> {
            Ok(self.1.lock().unwrap().iter().any(|r| r == row))
        }
    }

    fn task(rrule: &str) -> Task {
        Task {
            id: "t1".into(),
            title: "t".into(),
            prompt: "go".into(),
            rrule: rrule.into(),
            target: Target::Session {
                session: "c1".into(),
            },
            active: true,
            next_at: None,
            last_run_at: None,
            updated_at: 0,
            in_flight: None,
        }
    }

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let deliver: Arc<dyn Deliver> = Arc::new(Recorder::new());
        let saved = Schedules::open(dir.path(), deliver.clone())
            .save(&json!({ "task": {
                "title": "standup", "prompt": "post it",
                "rrule": "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY",
                "target": {"kind": "session", "session": "c1"},
            } }))
            .unwrap();
        assert!(!saved.id.is_empty() && saved.next_at.is_some());
        let tasks = Schedules::open(dir.path(), deliver).list();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, saved.id);
        assert_eq!(
            tasks[0].target,
            Target::Session {
                session: "c1".into()
            }
        );
    }

    #[test]
    fn missed_occurrences_fire_once() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(Recorder::new());
        let store = Schedules::open(dir.path(), hits.clone());
        let mut t = task("DTSTART:20260101T090000Z\nRRULE:FREQ=MINUTELY");
        t.next_at = Some(utc("2026-11-18T10:00:00Z").timestamp_millis());
        *store.tasks() = vec![t];
        let now = utc("2026-11-18T11:00:00Z"); // ~60 missed minutes
        store.tick(now);
        assert_eq!(hits.0.lock().unwrap().len(), 1); // latest missed fires once
        store.tick(now + chrono::Duration::seconds(30)); // next is 11:01
        assert_eq!(hits.0.lock().unwrap().len(), 1);
        hits.finish_all();
        store.tick(now + chrono::Duration::seconds(61));
        assert_eq!(hits.0.lock().unwrap().len(), 2);
    }

    const EVERY_15: &str = "DTSTART:20260101T090000Z\nRRULE:FREQ=MINUTELY;INTERVAL=15";

    /// A store holding `tasks`, each due at 09:00 on 2026-11-18.
    fn due_store(dir: &Path, hits: &Arc<Recorder>, tasks: Vec<Task>) -> Schedules {
        let store = Schedules::open(dir, hits.clone());
        *store.tasks() = tasks
            .into_iter()
            .map(|mut t| {
                t.next_at = Some(utc("2026-11-18T09:00:00Z").timestamp_millis());
                t
            })
            .collect();
        store
    }

    #[test]
    fn a_busy_target_gets_one_wake_per_pending_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(Recorder::new());
        let store = due_store(dir.path(), &hits, vec![task(EVERY_15)]);
        let at = |hhmm: &str| utc(&format!("2026-11-18T{hhmm}:00Z"));
        store.tick(at("09:00"));
        assert_eq!(hits.count(), 1);
        let first = store.list().remove(0);
        assert_eq!(first.in_flight.as_ref().unwrap().row, "row-1");
        // A one-hour turn: three more occurrences come due behind it.
        for hhmm in ["09:15", "09:30", "09:45"] {
            store.tick(at(hhmm));
        }
        assert_eq!(
            hits.count(),
            1,
            "occurrences coalesce into the pending wake"
        );
        let held = store.list().remove(0);
        assert_eq!(held.last_run_at, first.last_run_at, "nothing new ran");
        assert_eq!(held.next_at, Some(at("10:00").timestamp_millis()));
        assert_eq!(held.in_flight, first.in_flight);
        // The turn ends: the next occurrence wakes the target again.
        hits.finish_all();
        store.tick(at("10:00"));
        assert_eq!(hits.count(), 2);
        assert_eq!(store.list()[0].in_flight.as_ref().unwrap().row, "row-2");
    }

    #[test]
    fn separate_schedules_on_one_target_do_not_coalesce() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(Recorder::new());
        let mut other = task(EVERY_15);
        other.id = "t2".into();
        other.prompt = "check".into();
        let store = due_store(dir.path(), &hits, vec![task(EVERY_15), other]);
        store.tick(utc("2026-11-18T09:00:00Z"));
        assert_eq!(
            *hits.0.lock().unwrap(),
            vec!["t1:go".to_string(), "t2:check".to_string()]
        );
        store.tick(utc("2026-11-18T09:15:00Z"));
        assert_eq!(hits.count(), 2, "each task coalesces only into its own row");
    }

    #[test]
    fn in_flight_survives_restart_but_never_suppresses_for_good() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(Recorder::new());
        due_store(dir.path(), &hits, vec![task(EVERY_15)]).tick(utc("2026-11-18T09:00:00Z"));
        // Restart: the marker is read back and still coalesces while owed.
        let reopened = Schedules::open(dir.path(), hits.clone());
        assert_eq!(reopened.list()[0].in_flight.as_ref().unwrap().row, "row-1");
        reopened.tick(utc("2026-11-18T09:15:00Z"));
        assert_eq!(hits.count(), 1);
        // A crash lost the turn: the row is owed nothing, so the next
        // occurrence delivers.
        hits.finish_all();
        Schedules::open(dir.path(), hits.clone()).tick(utc("2026-11-18T09:30:00Z"));
        assert_eq!(hits.count(), 2);
        // A row the chat claims forever stops coalescing after a day.
        let stuck = Schedules::open(dir.path(), hits.clone());
        stuck.tick(utc("2026-11-19T09:30:00Z"));
        assert_eq!(hits.count(), 3, "a stale marker does not suppress");
    }

    #[test]
    fn a_retargeted_task_does_not_coalesce_into_the_old_chat() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(Recorder::new());
        let store = due_store(dir.path(), &hits, vec![task(EVERY_15)]);
        store.tick(utc("2026-11-18T09:00:00Z"));
        store.tasks()[0].target = Target::Session {
            session: "c2".into(),
        };
        store.tick(utc("2026-11-18T09:15:00Z"));
        assert_eq!(hits.count(), 2);
        assert_eq!(store.list()[0].in_flight.as_ref().unwrap().session, "c2");
    }

    #[test]
    fn run_now_bypasses_coalescing_and_becomes_the_pending_wake() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(Recorder::new());
        let store = due_store(dir.path(), &hits, vec![task(EVERY_15)]);
        store.tick(utc("2026-11-18T09:00:00Z"));
        store.run_now(&json!({ "id": "t1" })).unwrap();
        assert_eq!(hits.count(), 2, "a manual run always queues");
        assert_eq!(store.list()[0].in_flight.as_ref().unwrap().row, "row-2");
        store.tick(utc("2026-11-18T09:15:00Z"));
        assert_eq!(hits.count(), 2, "the scheduled occurrence folds into it");
    }

    #[test]
    fn save_keeps_the_engine_owned_marker() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(Recorder::new());
        let store = due_store(dir.path(), &hits, vec![task(EVERY_15)]);
        store.tick(utc("2026-11-18T09:00:00Z"));
        let saved = store
            .save(&json!({ "task": {
                "id": "t1", "title": "t", "prompt": "go again", "rrule": EVERY_15,
                "target": {"kind": "session", "session": "c1"},
                "inFlight": {"session": "c1", "row": "forged", "queuedAt": 0},
            } }))
            .unwrap();
        assert_eq!(saved.in_flight.unwrap().row, "row-1");
    }

    #[derive(Clone, Copy)]
    enum RosterSetup {
        OrdinaryArchived,
        LiveCaptain,
        HiddenCaptain,
        DemotedCaptain,
    }

    async fn boot_core(dir: &Path) -> crate::EngineCore {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("device-id"), "dev-schedules").unwrap();
        let registry = crate::HarnessRegistry::new();
        registry.register(Arc::new(zeron_harness::mock::MockHarness {
            script: vec![zeron_proto::AgentEvent::Done {
                status: zeron_proto::DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            }],
        }));
        let core = crate::EngineCore::assemble(
            dir,
            Arc::new(registry),
            zeron_proto::HarnessId::Mock,
            None,
        )
        .expect("engine core assembles");
        core.sessions.set_no_ipc_port();
        core
    }

    fn prompt_in_doc(core: &crate::EngineCore, chat: &str, prompt: &str) -> bool {
        core.doc_host
            .open(chat)
            .ok()
            .and_then(|h| h.doc().read_entries().ok())
            .unwrap_or_default()
            .iter()
            .any(|entry| {
                entry.role == zeron_doc::MessageRole::User
                    && entry.parts.iter().any(|part| {
                        matches!(part, zeron_doc::MessagePart::Text { text, .. } if text == prompt)
                    })
            })
    }

    async fn captain_lifecycle_case(setup: RosterSetup) -> (Task, bool, bool) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        let core = boot_core(&dir).await;
        let session = "chat-scheduled-captain";
        core.workspace
            .create_chat(session, None, Some(&core.device_id), None, None)
            .expect("target chat is created");
        match setup {
            RosterSetup::OrdinaryArchived => {
                core.workspace
                    .set_chat_archived(session, true)
                    .expect("ordinary chat archives");
            }
            RosterSetup::LiveCaptain => {
                core.captains
                    .promote(session, "Ada")
                    .expect("Captain promotes");
            }
            RosterSetup::HiddenCaptain => {
                core.captains
                    .promote(session, "Ada")
                    .expect("Captain promotes");
                core.captains
                    .archive(session, |_| false)
                    .expect("Captain hides");
                core.workspace
                    .set_chat_archived(session, true)
                    .expect("Captain chat archives");
            }
            RosterSetup::DemotedCaptain => {
                core.captains
                    .promote(session, "Ada")
                    .expect("Captain promotes");
                core.captains.demote(session).expect("Captain demotes");
            }
        }

        let schedules = Schedules::open(
            &dir,
            Arc::new(SessionDeliver {
                doc_host: core.doc_host.clone(),
                workspace: core.workspace.clone(),
                data_dir: dir.clone(),
            }),
        );
        let mut due = task("DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY");
        due.target = Target::Session {
            session: session.into(),
        };
        due.next_at = Some(utc("2026-11-18T09:00:00Z").timestamp_millis());
        *schedules.tasks() = vec![due];
        let now = utc("2026-11-18T09:00:00Z");
        schedules.tick(now);
        let stored = schedules.list().remove(0);
        let expects_delivery = matches!(
            setup,
            RosterSetup::OrdinaryArchived | RosterSetup::LiveCaptain
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut delivered = prompt_in_doc(&core, session, "go");
        while !delivered && tokio::time::Instant::now() < deadline {
            if !expects_delivery {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            delivered = prompt_in_doc(&core, session, "go");
        }
        if !expects_delivery {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            delivered = prompt_in_doc(&core, session, "go");
        }
        let row_archived = core
            .workspace
            .chat(session)
            .expect("workspace row reads")
            .expect("workspace row exists")
            .archived;
        core.shutdown().await;
        (stored, delivered, row_archived)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_live_captain_receives_a_scheduled_wake() {
        let (stored, delivered, _) = captain_lifecycle_case(RosterSetup::LiveCaptain).await;
        assert!(delivered, "the live Captain's prompt is queued");
        assert!(stored.active);
        assert_eq!(
            stored.last_run_at,
            Some(utc("2026-11-18T09:00:00Z").timestamp_millis())
        );
        assert!(
            stored.next_at.unwrap() > utc("2026-11-18T09:00:00Z").timestamp_millis(),
            "the recurrence advances after delivery"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hidden_captain_skips_the_occurrence_without_unarchiving() {
        let (stored, delivered, row_archived) =
            captain_lifecycle_case(RosterSetup::HiddenCaptain).await;
        assert!(!delivered, "a hidden Captain is not queued");
        assert!(row_archived, "the Captain chat row stays archived");
        assert!(stored.active, "sleep does not end the schedule");
        assert_eq!(stored.last_run_at, None, "nothing ran");
        assert!(
            stored.next_at.unwrap() > utc("2026-11-18T09:00:00Z").timestamp_millis(),
            "the skipped occurrence is consumed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_demoted_captain_ends_the_schedule() {
        let (stored, delivered, _) = captain_lifecycle_case(RosterSetup::DemotedCaptain).await;
        assert!(
            !delivered,
            "a demoted Captain is not queued as an ordinary chat"
        );
        assert!(!stored.active, "the schedule ends durably");
        assert_eq!(stored.next_at, None);
        assert_eq!(stored.last_run_at, None, "nothing ran");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_queued_wake_is_pending_until_its_row_leaves_the_queue() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        let core = boot_core(&dir).await;
        let session = "chat-busy-captain";
        core.workspace
            .create_chat(session, None, Some(&core.device_id), None, None)
            .expect("target chat is created");
        core.captains
            .promote(session, "Ada")
            .expect("Captain promotes");
        // Hold the queue as a busy turn would.
        core.doc_host.open(session).expect("chat doc opens");
        core.doc_host.pause_all_queues();
        let schedules = Schedules::open(
            &dir,
            Arc::new(SessionDeliver {
                doc_host: core.doc_host.clone(),
                workspace: core.workspace.clone(),
                data_dir: dir.clone(),
            }),
        );
        let mut due = task(EVERY_15);
        due.target = Target::Session {
            session: session.into(),
        };
        due.next_at = Some(utc("2026-11-18T09:00:00Z").timestamp_millis());
        *schedules.tasks() = vec![due];
        let queued = |core: &crate::EngineCore| {
            core.doc_host
                .open(session)
                .unwrap()
                .doc()
                .read_queue()
                .unwrap()
        };
        schedules.tick(utc("2026-11-18T09:00:00Z"));
        schedules.tick(utc("2026-11-18T09:15:00Z"));
        let rows = queued(&core);
        assert_eq!(rows.len(), 1, "one wake queued behind the busy turn");
        assert_eq!(
            schedules.list()[0].in_flight.as_ref().unwrap().row,
            rows[0].id
        );
        // The row leaves the queue with no turn running: owed nothing.
        assert!(
            core.doc_host
                .update_queued_message(session, &rows[0].id, "")
                .unwrap()
        );
        schedules.tick(utc("2026-11-18T09:30:00Z"));
        assert_eq!(queued(&core).len(), 1, "the next occurrence queues again");

        // Dispatched: the row is the latest user message. It is owed while
        // the turn it started runs, not once a later wake turn runs instead.
        let row = queued(&core)[0].id.clone();
        assert!(
            core.doc_host
                .update_queued_message(session, &row, "")
                .unwrap()
        );
        let user_row = |created_at: i64| zeron_doc::SessionMessageEntry {
            id: row.clone(),
            role: zeron_doc::MessageRole::User,
            parts: vec![zeron_doc::MessagePart::Text {
                id: new_id(),
                text: "go".into(),
            }],
            created_at,
            device_id: core.device_id.clone(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        };
        let doc = core.doc_host.open(session).unwrap();
        doc.doc().push_message(&user_row(now_ms())).unwrap();
        core.sessions
            .seed_status(session, zeron_proto::SessionStatus::Working);
        let deliver = SessionDeliver {
            doc_host: core.doc_host.clone(),
            workspace: core.workspace.clone(),
            data_dir: dir.clone(),
        };
        assert!(deliver.pending(session, &row).unwrap(), "its own turn runs");
        let stale = format!("{row}-old");
        let mut old = user_row(now_ms() - 3_600_000);
        old.id = stale.clone();
        doc.doc().push_message(&old).unwrap();
        assert!(
            !deliver.pending(session, &stale).unwrap(),
            "a turn started long after the row is not its turn"
        );
        // A demoted Captain owes no wake: the next occurrence ends the task.
        core.captains.demote(session).expect("Captain demotes");
        assert!(!deliver.pending(session, &row).unwrap());
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_ordinary_archived_session_still_delivers_and_unarchives() {
        let (stored, delivered, row_archived) =
            captain_lifecycle_case(RosterSetup::OrdinaryArchived).await;
        assert!(delivered, "ordinary session behavior is unchanged");
        assert!(
            !row_archived,
            "queue_message still unarchives ordinary chats"
        );
        assert!(stored.active);
    }
}
