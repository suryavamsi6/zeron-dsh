//! Engine-owned scheduled tasks: dsh's schedule service is a long-lived Host
//! service, but zeron's DeepSeek sidecar is per-run, so the engine keeps the
//! task store (`{data_dir}/schedules.json`, atomic temp+rename writes) and
//! fires each occurrence by queueing its prompt on a chat — the same internal
//! row `QueueMessage` appends.

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

/// One fired occurrence's delivery; the engine's [`SessionDeliver`] is what
/// runs in production, tests substitute a recorder.
pub trait Deliver: Send + Sync {
    /// Returns the chat the prompt landed on.
    fn deliver(&self, task: &Task) -> Result<String, EngineError>;
}

/// Queues a fired prompt on the target chat's doc — the row `QueueMessage`
/// appends — creating `new_once`/`new` chats first (`cwd`, this device, the
/// default harness: `Mutate`'s `CreateChat` without a space or a parent).
pub struct SessionDeliver {
    pub doc_host: crate::doc_host::DocHost,
    pub workspace: crate::workspace_host::WorkspaceHost,
}

impl Deliver for SessionDeliver {
    fn deliver(&self, task: &Task) -> Result<String, EngineError> {
        let session = match &task.target {
            Target::Session { session } => session.clone(),
            Target::NewOnce {
                session: Some(session),
                ..
            } => session.clone(),
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
        self.doc_host
            .queue_message(&session, &task.prompt, Vec::new())?;
        Ok(session)
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
    kick: tokio::sync::Notify,
}

/// dsh's delivery floor: at least a minute between one task's runs. Also the
/// runner's longest sleep, so a failed delivery retries within it.
const MIN_RUN_GAP_MS: i64 = 60_000;
const POLL_CAP: Duration = Duration::from_secs(60);

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
                kick: tokio::sync::Notify::new(),
            }),
        }
    }

    /// The wake loop: sleep to the earliest `nextAt` (never longer than
    /// [`POLL_CAP`]), fire what's due, repeat. No runtime, no runner — the
    /// store still answers RPCs.
    pub fn start(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let this = self.clone();
        runtime.spawn(async move {
            loop {
                let wait = this.tick(Utc::now());
                tokio::select! {
                    _ = tokio::time::sleep(wait.min(POLL_CAP)) => {}
                    _ = this.inner.kick.notified() => {}
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
            match self.inner.deliver.deliver(task) {
                Ok(session) => {
                    dirty = true;
                    if let Target::NewOnce { session: at, .. } | Target::New { session: at, .. } =
                        &mut task.target
                    {
                        *at = Some(session);
                    }
                    task.last_run_at = Some(now_ms);
                    task.updated_at = now_ms;
                    task.next_at = parse_rrule(&task.rrule)
                        .ok()
                        .and_then(|rule| rule.next_after(now))
                        .map(|t| t.timestamp_millis());
                    if task.next_at.is_none() {
                        task.active = false;
                    }
                    if let Some(next) = task.next_at {
                        wait = wait.min(to_dur(next - now_ms));
                    }
                }
                // `nextAt` stays: the next wake, at most POLL_CAP out, retries.
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
        task.last_run_at = tasks
            .iter()
            .find(|t| !task.id.is_empty() && t.id == task.id)
            .and_then(|t| t.last_run_at);
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
    /// owes that occurrence.
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
        let session = self
            .inner
            .deliver
            .deliver(task)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        if let Target::NewOnce { session: at, .. } | Target::New { session: at, .. } =
            &mut task.target
        {
            *at = Some(session.clone());
        }
        task.last_run_at = Some(now_ms());
        task.updated_at = now_ms();
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

    struct Recorder(Mutex<Vec<String>>);
    impl Deliver for Recorder {
        fn deliver(&self, task: &Task) -> Result<String, EngineError> {
            self.0
                .lock()
                .unwrap()
                .push(format!("{}:{}", task.id, task.prompt));
            Ok("chat-1".into())
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
        }
    }

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let deliver: Arc<dyn Deliver> = Arc::new(Recorder(Mutex::new(vec![])));
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
        let hits = Arc::new(Recorder(Mutex::new(vec![])));
        let store = Schedules::open(dir.path(), hits.clone());
        let mut t = task("DTSTART:20260101T090000Z\nRRULE:FREQ=MINUTELY");
        t.next_at = Some(utc("2026-11-18T10:00:00Z").timestamp_millis());
        *store.tasks() = vec![t];
        let now = utc("2026-11-18T11:00:00Z"); // ~60 missed minutes
        store.tick(now);
        assert_eq!(hits.0.lock().unwrap().len(), 1); // latest missed fires once
        store.tick(now + chrono::Duration::seconds(30)); // next is 11:01
        assert_eq!(hits.0.lock().unwrap().len(), 1);
        store.tick(now + chrono::Duration::seconds(61));
        assert_eq!(hits.0.lock().unwrap().len(), 2);
    }
}
