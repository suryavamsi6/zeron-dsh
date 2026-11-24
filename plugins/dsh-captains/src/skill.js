//! The Captain's `captain-digest` skill: the how-to the always-on role prompt
//! keeps short (SUR-102). Registered as a runtime skill, Captain rows only.

/** The skill's id: the name the model and the `$` menu look up. */
export const SKILL_NAME = 'captain-digest';

/** One-line description, shown in the catalog before the body is loaded. */
export const SKILL_DESCRIPTION = 'How a Captain keeps DIGEST.md, triages crew reports and writes handoffs';

/** The how-to body the model reads when it loads the skill on demand. */
export const captainDigestContent = `# Captain digest

Your Drive is the crew's memory. Its layout:

- AGENTS.md: your rules.
- NOTES.md: the plan.
- DIGEST.md: the state of the crew, rewritten after every report.
- CREW.md: the rules every crew chat reads.
- reports/: what crew chats file.
- handoffs/: briefs you write for them.

At the start of every turn, read AGENTS.md and NOTES.md with
captain_drive_read before doing anything else.

## Crew lifecycle

Start one crew chat per piece of work with create_chat (kind: subagent),
pointing it at its handoff. When the work is done, archive_chat the crew
chat so the roster of live crew stays exact.

Messages starting [crew report] come from your crew. After each [crew
report], rewrite DIGEST.md with captain_drive_write so the digest always
shows the state you just learned.

## DIGEST.md

Keep DIGEST.md to exactly this shape:

    ## Needs you
    - anything that needs the person: a decision, a credential, a blocker.
      One bullet per item.
    ## New
    - crew reports you have read but not yet acted on: which chat filed
      it and what it changes.
    ## In progress
    - live crew work: the owning chat, its goal, its last known state.

Needs you holds what only the person can settle; repeat every bullet under
it at the end of your reply so the person cannot miss it. New drains as
you act on each report. In progress tracks what the crew owes you. Delete
an item the moment it stops being true — a stale digest lies to the next
turn.

## Handoffs

Write each brief to handoffs/<yyyy-mm-dd>-<slug>.md, where <slug> is a few
lowercase words naming the work. A handoff carries four fields:

- Goal: the one outcome the crew chat exists to reach.
- State: what is already true — files touched, decisions made, dead ends.
- Next: the immediate steps, in order.
- Links: the reports, files and chats the crew must read first.

When the work's Goal, State or Next moves, rewrite the handoff; a stale
brief sends crew down dead ends.
`;

/** The catalog entry the provider lists. */
export const captainDigestSkill = {
  name: SKILL_NAME,
  description: SKILL_DESCRIPTION,
  invocation: { modelInvocable: true, userInvocable: true },
  source: 'runtime',
  provider: 'zeron-captains',
};

/** Rank a runtime skill above the bundled ones (lower wins). */
export const RUNTIME_SKILL_RANK = 400;

/** The Cordis skill provider the plugin registers for a Captain row. */
export const captainDigestProvider = () => ({
  name: 'zeron-captains',
  list: () => Promise.resolve([{ ...captainDigestSkill, rank: RUNTIME_SKILL_RANK, locator: SKILL_NAME }]),
  get: async () => ({ ...captainDigestSkill, content: captainDigestContent }),
});
