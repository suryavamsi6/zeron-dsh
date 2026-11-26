import { describe, expect, it } from "vitest";
import {
  MAX_WAKEUP_ATTEMPTS,
  WAKEUP_ACK_TIMEOUT_MS,
  nextAfter,
  parseRrule,
  planFire,
  wakeupRetryDue,
  type CloudSchedule,
  type CloudWakeup
} from "./schedules";

/** The RFC 3339 second the rule fires next at, "" when spent. */
const next = (src: string, after: string): string => {
  const t = nextAfter(parseRrule(src), Date.parse(after));
  return t === undefined ? "" : new Date(t).toISOString().replace(".000Z", "Z");
};

describe("parseRrule/nextAfter (parity with crates/engine/src/schedules.rs)", () => {
  it("daily at nine and weekdays by BYDAY", () => {
    const daily = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY";
    expect(next(daily, "2026-01-05T08:59:00Z")).toBe("2026-01-05T09:00:00Z");
    expect(next(daily, "2026-01-05T09:00:00Z")).toBe("2026-01-06T09:00:00Z");
    const weekdays = "DTSTART:20260105T090000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR;BYHOUR=9;BYMINUTE=0";
    expect(next(weekdays, "2026-01-05T09:00:00Z")).toBe("2026-01-07T09:00:00Z");
    expect(next(weekdays, "2026-01-07T09:00:00Z")).toBe("2026-01-09T09:00:00Z");
    expect(next(weekdays, "2026-01-09T09:00:00Z")).toBe("2026-01-12T09:00:00Z");
  });

  it("fixed-offset TZID; named zones refused", () => {
    const offset = "DTSTART;TZID=+05:30:20260101T090000\nRRULE:FREQ=DAILY";
    expect(next(offset, "2026-01-04T10:00:00Z")).toBe("2026-01-05T03:30:00Z");
    expect(() => parseRrule("DTSTART;TZID=America/New_York:20260101T090000\nRRULE:FREQ=DAILY")).toThrow(/named zones/);
  });

  it("COUNT and UNTIL end the rule", () => {
    const count = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY;COUNT=3";
    expect(next(count, "2026-01-03T08:00:00Z")).toBe("2026-01-03T09:00:00Z");
    expect(next(count, "2026-01-03T09:00:00Z")).toBe("");
    const until = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY;UNTIL=20260103T090000Z";
    expect(next(until, "2026-01-02T10:00:00Z")).toBe("2026-01-03T09:00:00Z");
    expect(next(until, "2026-01-03T09:00:00Z")).toBe("");
  });

  it("minutely and hourly expand within their periods", () => {
    const minutely = "DTSTART:20260101T090000Z\nRRULE:FREQ=MINUTELY;INTERVAL=15";
    expect(next(minutely, "2026-01-01T09:00:00Z")).toBe("2026-01-01T09:15:00Z");
    const hourly = "DTSTART:20260101T090000Z\nRRULE:FREQ=HOURLY;BYMINUTE=30";
    expect(next(hourly, "2026-01-01T09:00:00Z")).toBe("2026-01-01T09:30:00Z");
    expect(next(hourly, "2026-01-01T09:30:00Z")).toBe("2026-01-01T10:30:00Z");
  });

  it("a DTSTART exactly at `after` is consumed, the next occurrence answers", () => {
    const daily = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY";
    expect(next(daily, "2026-01-01T09:00:00Z")).toBe("2026-01-02T09:00:00Z");
  });
});

const scheduleRow = (over: Partial<CloudSchedule> = {}): CloudSchedule => ({
  id: "s1",
  chat_id: "c1",
  title: "standup",
  prompt: "go",
  rrule: "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY",
  active: 1,
  next_fire_at: Date.parse("2026-01-05T09:00:00Z"),
  last_fired_at: null,
  last_error: null,
  created_at: 0,
  updated_at: 0,
  ...over
});

describe("planFire", () => {
  const now = Date.parse("2026-01-05T09:00:30Z");

  it("fires a due occurrence and computes the next one", () => {
    const plan = planFire(scheduleRow(), now, false);
    expect(plan.kind).toBe("fire");
    expect(plan.nextFireAt).toBe(Date.parse("2026-01-06T09:00:00Z"));
  });

  it("coalesces into a wake-up still in flight — one wake, never two", () => {
    const plan = planFire(scheduleRow(), now, true);
    expect(plan.kind).toBe("coalesce");
    // The occurrence is still consumed: the schedule advances instead of piling up.
    expect(plan.nextFireAt).toBe(Date.parse("2026-01-06T09:00:00Z"));
  });

  it("a spent rule still owes the due occurrence; no next fire ends it", () => {
    const row = scheduleRow({ rrule: "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY;COUNT=1" });
    const plan = planFire(row, now, false);
    expect(plan.kind).toBe("fire");
    expect(plan.nextFireAt).toBeUndefined();
  });

  it("ends a schedule whose rule no longer parses, with the error", () => {
    const plan = planFire(scheduleRow({ rrule: "garbage" }), now, false);
    expect(plan.kind).toBe("end");
    if (plan.kind === "end") expect(plan.error).toBeTruthy();
  });
});

const wakeupRow = (over: Partial<CloudWakeup> = {}): CloudWakeup => ({
  fire_id: "f1",
  schedule_id: "s1",
  chat_id: "c1",
  occurrence_at: 0,
  prompt: "go",
  state: "pending",
  attempts: 0,
  created_at: 1_000_000,
  nudged_at: 1_000_000,
  acked_at: null,
  ...over
});

describe("wakeupRetryDue", () => {
  it("a fresh wake-up is not retried inside the ack window", () => {
    expect(wakeupRetryDue(wakeupRow(), 1_000_000 + WAKEUP_ACK_TIMEOUT_MS - 1)).toBe(false);
  });

  it("an unacked wake-up past the ack window rewakes", () => {
    expect(wakeupRetryDue(wakeupRow(), 1_000_000 + WAKEUP_ACK_TIMEOUT_MS)).toBe(true);
  });

  it("acked and dropped wake-ups never rewake", () => {
    const at = 1_000_000 + 60 * 60_000;
    expect(wakeupRetryDue(wakeupRow({ state: "acked" }), at)).toBe(false);
    expect(wakeupRetryDue(wakeupRow({ state: "dropped" }), at)).toBe(false);
  });

  it("exhausted attempts never rewake — the occurrence is dropped, not backlogged", () => {
    expect(wakeupRetryDue(wakeupRow({ attempts: MAX_WAKEUP_ATTEMPTS }), 1_000_000 + 24 * 3_600_000)).toBe(false);
  });

  it("the backoff grows with attempts", () => {
    const w = wakeupRow({ attempts: 2 });
    // Backoff for attempt 2: min(cap, timeout * 4) — not due right at the base timeout.
    expect(wakeupRetryDue(w, 1_000_000 + WAKEUP_ACK_TIMEOUT_MS + 1)).toBe(false);
    expect(wakeupRetryDue(w, 1_000_000 + WAKEUP_ACK_TIMEOUT_MS * 4)).toBe(true);
  });
});
