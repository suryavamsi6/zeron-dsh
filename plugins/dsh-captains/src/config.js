import { isAbsolute } from 'node:path';

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
