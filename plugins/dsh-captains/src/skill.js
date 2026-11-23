//! The Captain's `captain-digest` skill: the how-to the always-on role prompt
//! keeps short (SUR-102). Registered as a runtime skill, Captain rows only.

/** The skill's id: the name the model and the `$` menu look up. */
export const SKILL_NAME = 'captain-digest';

/** One-line description, shown in the catalog before the body is loaded. */
export const SKILL_DESCRIPTION = 'How a Captain keeps DIGEST.md, triages crew reports and writes handoffs';

/** The how-to body. STUB: the next brief writes it. */
export const captainDigestContent = '';

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
