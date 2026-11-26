/**
 * Edge-owned Captain schedules (SUR-268, docs/design/cloud-device.md):
 * the CloudAccount DO is the system of record for schedules whose target
 * chat is cloud-hosted, because the local runner fires only while its
 * engine process runs — a cloud Captain's engine is exactly the one that
 * sleeps.
 *
 * Firing cannot enqueue into the synced chat doc: chat-room is an opaque
 * blob relay by design (all CRDT semantics live in the clients), so the
 * edge holds each fired occurrence as a durable wake-up row instead. The
 * hosting engine pulls pending wake-ups over its authenticated runner
 * channel, materializes them as ordinary queued messages (the doc write
 * only a host can make), and acks. Unacked wake-ups rewake the machine
 * with backoff; an exhausted occurrence is dropped, never backlogged —
 * latest-only, matching the local runner's missed-occurrence collapse.
 *
 * Pure of Workers APIs where it can be (the Node unit tier covers the
 * rule math and fire planning directly); the DO integration lives in
 * cloud-account.ts.
 */

// ── rrule subset (mirrors crates/engine/src/schedules.rs) ──────────────────

export type Freq = "MINUTELY" | "HOURLY" | "DAILY" | "WEEKLY";

export interface Rule {
  /** Fixed UTC offset in seconds (a named zone is refused, not guessed). */
  zoneSeconds: number;
  /** DTSTART's wall time, epoch ms as read in the fixed offset. */
  startWallMs: number;
  untilWallMs?: number;
  freq: Freq;
  interval: number;
  days?: number[]; // 0 = Monday .. 6 = Sunday
  hours?: number[];
  minutes?: number[];
  count?: number;
  /** Wall-clock second-of-minute every occurrence carries. */
  second: number;
}

const DAY_S = 86_400;
const WEEK_S = 604_800;

const parseNaive = (value: string, what: string): number => {
  const m = /^(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})$/.exec(value);
  if (!m) throw new Error(`${what} must be YYYYMMDDTHHMMSS`);
  const [, y, mo, d, h, mi, s] = m;
  // Wall time represented as if UTC: all period math happens in this frame.
  // Date.UTC rolls impossible dates forward (2027-02-29 → Mar 1), silently
  // moving the whole schedule — round-trip the components and refuse instead,
  // as chrono's strict parse does in the engine's copy of this subset.
  const ms = Date.UTC(+y, +mo - 1, +d, +h, +mi, +s);
  const back = new Date(ms);
  if (
    back.getUTCFullYear() !== +y || back.getUTCMonth() !== +mo - 1 || back.getUTCDate() !== +d ||
    back.getUTCHours() !== +h || back.getUTCMinutes() !== +mi || back.getUTCSeconds() !== +s
  ) {
    throw new Error(`${what} is not a real date-time`);
  }
  return ms;
};

const wallParts = (
  wallMs: number
): { y: number; mo: number; d: number; h: number; mi: number; s: number; weekday: number } => {
  const t = new Date(wallMs);
  return {
    y: t.getUTCFullYear(),
    mo: t.getUTCMonth(),
    d: t.getUTCDate(),
    h: t.getUTCHours(),
    mi: t.getUTCMinutes(),
    s: t.getUTCSeconds(),
    // JS: 0 = Sunday; the rule frame wants 0 = Monday.
    weekday: (t.getUTCDay() + 6) % 7
  };
};

const DAY_NAMES: Record<string, number> = { MO: 0, TU: 1, WE: 2, TH: 3, FR: 4, SA: 5, SU: 6 };

export function parseRrule(src: string): Rule {
  const lines = src
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  if (lines.length !== 2) throw new Error("rrule is a DTSTART line and an RRULE line");
  const [dtstart, rrule] = lines;
  const colon = dtstart.lastIndexOf(":");
  if (colon < 0) throw new Error("DTSTART has no value");
  const head = dtstart.slice(0, colon);
  const value = dtstart.slice(colon + 1);
  if (!head.startsWith("DTSTART")) throw new Error("first line must be DTSTART");
  const zulu = value.endsWith("Z");
  const params = head.slice("DTSTART".length);
  let zoneSeconds = 0;
  if (params !== "") {
    const tzid = params.startsWith(";TZID=") ? params.slice(";TZID=".length) : undefined;
    if (tzid === undefined) throw new Error("bad DTSTART params");
    if (["UTC", "GMT", "Z", "Etc/UTC", "Etc/GMT", "Etc/Universal"].includes(tzid)) {
      zoneSeconds = 0;
    } else {
      const sign = tzid.startsWith("+") ? 1 : tzid.startsWith("-") ? -1 : 0;
      if (sign === 0) {
        throw new Error(
          `TZID ${JSON.stringify(tzid)}: named zones need chrono-tz, which zeron doesn't carry — use UTC or a fixed offset like +05:30`
        );
      }
      const digits = tzid.slice(1);
      const [hh, mm] = digits.includes(":") ? digits.split(":") : digits.length === 4 ? [digits.slice(0, 2), digits.slice(2)] : [digits, "0"];
      const h = Number(hh);
      const m = Number(mm);
      if (!Number.isInteger(h) || h < 0 || h >= 24 || !Number.isInteger(m) || m < 0 || m >= 60) {
        throw new Error("bad TZID offset");
      }
      zoneSeconds = sign * (h * 3600 + m * 60);
    }
  }
  if (zulu && zoneSeconds !== 0) throw new Error("DTSTART: 'Z' with a non-UTC TZID");
  const startWallMs = parseNaive(zulu ? value.slice(0, -1) : value, "DTSTART");

  if (!rrule.startsWith("RRULE:")) throw new Error("second line must be RRULE:");
  const rule: Rule = {
    zoneSeconds,
    startWallMs,
    freq: "DAILY",
    interval: 1,
    second: wallParts(startWallMs).s
  };
  let freqSeen = false;
  for (const part of rrule.slice("RRULE:".length).split(";")) {
    const eq = part.indexOf("=");
    if (eq < 0) throw new Error(`bad RRULE part ${JSON.stringify(part)}`);
    const key = part.slice(0, eq);
    const v = part.slice(eq + 1);
    switch (key) {
      case "FREQ": {
        if (freqSeen) throw new Error(`unsupported RRULE part ${part}`);
        if (!["MINUTELY", "HOURLY", "DAILY", "WEEKLY"].includes(v)) throw new Error(`unsupported FREQ=${v}`);
        rule.freq = v as Freq;
        freqSeen = true;
        break;
      }
      case "INTERVAL": {
        const n = Number(v);
        if (!Number.isInteger(n) || n < 1) throw new Error("INTERVAL must be a positive integer");
        rule.interval = n;
        break;
      }
      case "BYDAY":
        rule.days = v.split(",").map((d) => {
          const day = DAY_NAMES[d];
          if (day === undefined) throw new Error(`BYDAY ${JSON.stringify(d)}: only bare weekdays (MO..SU)`);
          return day;
        });
        break;
      case "BYHOUR":
      case "BYMINUTE": {
        const bound = key === "BYHOUR" ? 24 : 60;
        const nums = v.split(",").map((n) => {
          const x = Number(n);
          if (!Number.isInteger(x) || x < 0 || x >= bound) throw new Error(`${key} values are under ${bound}`);
          return x;
        });
        if (key === "BYHOUR") rule.hours = nums;
        else rule.minutes = nums;
        break;
      }
      case "COUNT": {
        const n = Number(v);
        if (!Number.isInteger(n) || n < 1) throw new Error("COUNT must be a positive integer");
        rule.count = n;
        break;
      }
      case "UNTIL": {
        // A Z-marked UNTIL is an instant: re-express it in the rule's offset.
        rule.untilWallMs = v.endsWith("Z")
          ? parseNaive(v.slice(0, -1), "UNTIL") + zoneSeconds * 1000
          : parseNaive(v, "UNTIL");
        break;
      }
      default:
        throw new Error(`unsupported RRULE part ${key}`);
    }
  }
  if (!freqSeen) throw new Error("RRULE needs FREQ");
  return rule;
}

/** Wall-time start of the period covering DTSTART. */
const baseOf = (rule: Rule): number => {
  const p = wallParts(rule.startWallMs);
  const dayStart = Date.UTC(p.y, p.mo, p.d);
  switch (rule.freq) {
    case "MINUTELY":
      return rule.startWallMs - p.s * 1000;
    case "HOURLY":
      return rule.startWallMs - (p.mi * 60 + p.s) * 1000;
    case "DAILY":
      return dayStart;
    case "WEEKLY":
      return dayStart - p.weekday * DAY_S * 1000;
  }
};

const spanSeconds = (freq: Freq): number =>
  freq === "MINUTELY" ? 60 : freq === "HOURLY" ? 3_600 : freq === "DAILY" ? DAY_S : WEEK_S;

/** The occurrences inside period `n` (wall ms), sorted, none before DTSTART. */
const expandPeriod = (rule: Rule, n: number): number[] => {
  const base = baseOf(rule);
  const step = rule.interval * n;
  const secondMs = rule.second * 1000;
  const inDay = (dayStartMs: number): number[] => {
    const p = wallParts(rule.startWallMs);
    const hours = rule.hours ?? [p.h];
    const minutes = rule.minutes ?? [p.mi];
    const out: number[] = [];
    for (const h of hours) {
      for (const mi of minutes) {
        const t = dayStartMs + h * 3_600_000 + mi * 60_000 + secondMs;
        if (t >= rule.startWallMs) out.push(t);
      }
    }
    return out.sort((a, b) => a - b);
  };
  const dayOk = (t: number): boolean => rule.days === undefined || rule.days.includes(wallParts(t).weekday);
  const hourOk = (t: number): boolean => rule.hours === undefined || rule.hours.includes(wallParts(t).h);
  const minOk = (t: number): boolean => rule.minutes === undefined || rule.minutes.includes(wallParts(t).mi);

  if (rule.freq === "MINUTELY") {
    const t = base + step * 60_000 + secondMs;
    return t >= rule.startWallMs && dayOk(t) && hourOk(t) && minOk(t) ? [t] : [];
  }
  if (rule.freq === "HOURLY") {
    const h0 = base + step * 3_600_000;
    if (!dayOk(h0) || !hourOk(h0)) return [];
    const hour = wallParts(h0).h;
    const dayStart = Date.UTC(...((): [number, number, number] => {
      const p = wallParts(h0);
      return [p.y, p.mo, p.d];
    })());
    const minutes = rule.minutes ?? [wallParts(rule.startWallMs).mi];
    return minutes
      .map((mi) => dayStart + hour * 3_600_000 + mi * 60_000 + secondMs)
      .filter((t) => t >= rule.startWallMs)
      .sort((a, b) => a - b);
  }
  if (rule.freq === "DAILY") {
    const dayStart = base + step * DAY_S * 1000;
    if (!dayOk(dayStart)) return [];
    return inDay(dayStart);
  }
  // WEEKLY
  const weekStart = base + step * WEEK_S * 1000;
  const days = rule.days ?? [wallParts(rule.startWallMs).weekday];
  const out: number[] = [];
  for (const d of days) {
    out.push(...inDay(weekStart + d * DAY_S * 1000));
  }
  return out.sort((a, b) => a - b);
};

/** Convert a wall-time frame ms to a real UTC epoch ms. */
const toUtc = (rule: Rule, wallMs: number): number => wallMs - rule.zoneSeconds * 1000;

/**
 * The first occurrence strictly after `afterMs` (a real UTC epoch ms), or
 * undefined once the rule is spent (COUNT) or over (UNTIL). Missed
 * occurrences collapse into the next one — never a backlog replay.
 */
export function nextAfter(rule: Rule, afterMs: number): number | undefined {
  // Read `after` in the rule's wall frame.
  const afterWall = afterMs + rule.zoneSeconds * 1000;
  let seen = 0;
  let last = rule.startWallMs - 1;
  let pending: number[] = [rule.startWallMs];
  const span = spanSeconds(rule.freq);
  let period =
    rule.count !== undefined
      ? 0 // COUNT tallies every occurrence: walk from DTSTART.
      : Math.max(0, Math.floor((afterWall - baseOf(rule)) / 1000 / span / rule.interval));
  while (period <= 4_000_000) {
    if (pending.length === 0) {
      pending = expandPeriod(rule, period);
      period += 1;
      continue;
    }
    const t = pending.shift()!;
    if (t === last) continue;
    last = t;
    if (rule.untilWallMs !== undefined && t > rule.untilWallMs) return undefined;
    seen += 1;
    if (rule.count !== undefined && seen > rule.count) return undefined;
    if (t > afterWall) return toUtc(rule, t);
  }
  return undefined;
}

// ── stored rows ─────────────────────────────────────────────────────────────

export interface CloudSchedule extends Record<string, SqlStorageValue> {
  id: string;
  chat_id: string;
  title: string;
  prompt: string;
  rrule: string;
  active: number; // 1/0 — SQLite
  next_fire_at: number | null;
  last_fired_at: number | null;
  last_error: string | null;
  created_at: number;
  updated_at: number;
}

export type WakeupState = "pending" | "acked" | "dropped";

export interface CloudWakeup extends Record<string, SqlStorageValue> {
  fire_id: string;
  schedule_id: string;
  chat_id: string;
  /** The schedule's occurrence time this wake-up is for. */
  occurrence_at: number;
  prompt: string;
  state: WakeupState;
  attempts: number;
  created_at: number;
  /** Last time the machine was (re)woken for it. */
  nudged_at: number | null;
  acked_at: number | null;
}

export const MAX_SCHEDULES_PER_CHAT = 32;
/** An ack this late means the wake never took: rewake, with backoff. */
export const WAKEUP_ACK_TIMEOUT_MS = 5 * 60_000;
/** Rewake backoff cap. */
export const WAKEUP_RETRY_CAP_MS = 30 * 60_000;
/** Past this many wake attempts the occurrence is dropped, never backlogged. */
export const MAX_WAKEUP_ATTEMPTS = 8;
/** The store never answers with rows older than this. */
export const WAKEUP_RETENTION_MS = 7 * 24 * 3_600_000;

export function ensureCloudSchedules(sql: SqlStorage): void {
  sql.exec(
    "CREATE TABLE IF NOT EXISTS cloud_schedule (" +
      "id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, title TEXT NOT NULL, prompt TEXT NOT NULL, rrule TEXT NOT NULL, " +
      "active INTEGER NOT NULL, next_fire_at INTEGER, last_fired_at INTEGER, last_error TEXT, " +
      "created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)"
  );
  sql.exec("CREATE INDEX IF NOT EXISTS cloud_schedule_due ON cloud_schedule(active, next_fire_at)");
  sql.exec(
    "CREATE TABLE IF NOT EXISTS cloud_wakeup (" +
      "fire_id TEXT PRIMARY KEY, schedule_id TEXT NOT NULL, chat_id TEXT NOT NULL, occurrence_at INTEGER NOT NULL, " +
      "prompt TEXT NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL, created_at INTEGER NOT NULL, " +
      "nudged_at INTEGER, acked_at INTEGER)"
  );
  sql.exec("CREATE INDEX IF NOT EXISTS cloud_wakeup_pending ON cloud_wakeup(state, chat_id)");
}

export function listSchedules(sql: SqlStorage, chatId?: string): CloudSchedule[] {
  return chatId === undefined
    ? ([...sql.exec("SELECT * FROM cloud_schedule ORDER BY created_at, id")] as CloudSchedule[])
    : ([...sql.exec("SELECT * FROM cloud_schedule WHERE chat_id=? ORDER BY created_at, id", chatId)] as CloudSchedule[]);
}

/** What one alarm pass should do with a schedule due at `now`. Pure. */
export type FirePlan =
  | { readonly kind: "fire"; readonly occurrenceAt: number; readonly nextFireAt?: number }
  /** A prior occurrence is still in flight: consume this one silently. */
  | { readonly kind: "coalesce"; readonly nextFireAt?: number }
  /** The rrule no longer parses: the schedule ends. */
  | { readonly kind: "end"; readonly error?: string; readonly nextFireAt?: number };

export function planFire(row: CloudSchedule, now: number, pendingWakeup: boolean): FirePlan {
  let rule: Rule;
  try {
    rule = parseRrule(row.rrule);
  } catch (e) {
    return { kind: "end", error: String(e instanceof Error ? e.message : e) };
  }
  const nextFireAt = nextAfter(rule, now);
  // A spent rule still owes the occurrence that is due now — `nextFireAt`
  // undefined is how the caller knows to end the schedule after it.
  if (pendingWakeup) return { kind: "coalesce", nextFireAt };
  return { kind: "fire", occurrenceAt: row.next_fire_at ?? now, nextFireAt };
}

/** Whether an unacked wake-up should rewake its machine now, and the backoff. */
export function wakeupRetryDue(w: CloudWakeup, now: number): boolean {
  if (w.state !== "pending" || w.attempts >= MAX_WAKEUP_ATTEMPTS) return false;
  const since = now - (w.nudged_at ?? w.created_at);
  const backoff = Math.min(WAKEUP_RETRY_CAP_MS, WAKEUP_ACK_TIMEOUT_MS * 2 ** Math.min(w.attempts, 3));
  return since >= Math.max(WAKEUP_ACK_TIMEOUT_MS, backoff);
}
