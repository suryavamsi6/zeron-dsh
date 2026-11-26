import { describe, expect, it } from "vitest";
import { call, createSession, enableCloud, newUser, userBearer } from "./cloud-helpers";

const RULE = "DTSTART:20990101T090000Z\nRRULE:FREQ=DAILY";

const save = (orgId: string, chatId: string, bearer: string, body: Record<string, unknown>) =>
  call("POST", `/cloud/${orgId}/sessions/${chatId}/schedules`, bearer, body);

describe("cloud schedules", () => {
  it("replaces a row only at the revision the caller loaded", async () => {
    const u = newUser("sched");
    const { spaceId } = await enableCloud(u);
    const chatId = "chat-schedules";
    expect((await createSession(u, chatId, spaceId)).status).toBe(200);
    const bearer = userBearer(u);

    const created = await save(u.orgId, chatId, bearer, {
      title: "Standup",
      prompt: "Summarize",
      rrule: RULE,
      active: true
    });
    expect(created.status, await created.clone().text()).toBe(200);
    const { schedule } = (await created.json()) as { schedule: { id: string; updated_at: number } };

    const stale = await save(u.orgId, chatId, bearer, {
      id: schedule.id,
      title: "Stale edit",
      prompt: "Summarize",
      rrule: RULE,
      active: false,
      expectedUpdatedAt: schedule.updated_at - 1
    });
    expect(stale.status).toBe(409);

    const current = await save(u.orgId, chatId, bearer, {
      id: schedule.id,
      title: "Current edit",
      prompt: "Summarize",
      rrule: RULE,
      active: false,
      expectedUpdatedAt: schedule.updated_at
    });
    expect(current.status, await current.clone().text()).toBe(200);
    const { schedule: replaced } = (await current.json()) as {
      schedule: { title: string; active: number; updated_at: number };
    };
    expect(replaced.title).toBe("Current edit");
    expect(replaced.active).toBe(0);
    expect(replaced.updated_at).toBeGreaterThan(schedule.updated_at);
  });
});
