import { isAbsolute } from 'node:path';
import { captainPrompt } from './src/prompts.js';

export const name = 'zeron-captains';
export const inject = ['systemPrompt'];

const roles = new Set(['none', 'captain', 'crew']);
const idPattern = /^[A-Za-z0-9_-]{1,128}$/;

export function normalizeConfig(raw) {
  // The bundle loads in every chat, so a row stays inert until the engine
  // stamps a role into it; anything the schema does not know throws before a
  // single registration can run.
  const { role = 'none', chatId, captainId, captainName, driveRoot } = raw ?? {};
  if (!roles.has(role)) {
    throw new Error('zeron-captains: unknown role');
  }
  if (role === 'none') {
    return { role, chatId, captainId, captainName, driveRoot };
  }
  if (typeof chatId !== 'string' || !idPattern.test(chatId)) {
    throw new Error('zeron-captains: chatId must be 1-128 characters of [A-Za-z0-9_-]');
  }
  if (typeof captainId !== 'string' || !idPattern.test(captainId)) {
    throw new Error('zeron-captains: captainId must be 1-128 characters of [A-Za-z0-9_-]');
  }
  if (typeof captainName !== 'string' || captainName.length < 1 || captainName.length > 80) {
    throw new Error('zeron-captains: captainName must be a string of 1-80 characters');
  }
  if (typeof driveRoot !== 'string' || !isAbsolute(driveRoot)) {
    throw new Error('zeron-captains: driveRoot must be an absolute path');
  }
  // A Captain is its own chat; a crew row names the Captain it answers to.
  if (role === 'captain' && captainId !== chatId) {
    throw new Error('zeron-captains: captainId must equal chatId when role is captain');
  }
  return { role, chatId, captainId, captainName, driveRoot };
}

// SUR-81 creates the folder engine-side; the row's driveRoot is the Drive path here (same decision as SUR-99's reuse line).
const driveDir = cfg => cfg.driveRoot;

export function apply(ctx, config) {
  const cfg = normalizeConfig(config);
  if (cfg.role !== 'captain') return;
  // The plugin context is process-wide, so a section registered at load would
  // reach every native child session too. Wait for `agent/created` instead and
  // register into each created agent's own scope, skipping subagents: the
  // Captain row belongs to the root agent alone.
  ctx.on('agent/created', ({ agent }) => {
    const scope = agent?.ctx;
    if (!scope?.systemPrompt || agent.session?.header?.origin === 'subagent') return;
    scope.effect(() => scope.systemPrompt.section({
      name: 'zeron-captains:role',
      order: scope.systemPrompt.getSectionOrder('DEPLOYMENT_PERSONA_SUFFIX') - 1,
      text: () => captainPrompt(cfg, driveDir(cfg)),
      interpolate: false,
    }));
  });
}
