import assert from 'node:assert/strict';
import { test } from 'node:test';
import { normalizeConfig } from '../src/config.js';

// SUR-193: the engine and this plugin must measure a Captain name in the same
// unit. JS `String.length` counts UTF-16 code units, so an astral character is
// two of them: 41 astral characters are 82 units even though Rust's
// `chars().count()` sees 41. The engine's `valid_captain_name` is pinned to this
// same 40/41 boundary, so a name this file accepts is a name the engine accepts.

const captain = captainName => ({
  role: 'captain',
  chatId: 'captain-1',
  captainId: 'captain-1',
  captainName,
  driveRoot: '/tmp/zeron-captains-drive',
});

test('the Captain name boundary is 80 UTF-16 code units', () => {
  const atTheLimit = '🚀'.repeat(40);
  assert.equal(atTheLimit.length, 80, '40 astral characters are 80 UTF-16 units');
  assert.doesNotThrow(() => normalizeConfig(captain(atTheLimit)));
  assert.equal(normalizeConfig(captain(atTheLimit)).captainName, atTheLimit);

  const overTheLimit = '🚀'.repeat(41);
  assert.equal(overTheLimit.length, 82, '41 astral characters are 82 UTF-16 units');
  assert.throws(
    () => normalizeConfig(captain(overTheLimit)),
    /captainName must be a string of 1-80 characters/,
    'the boundary this plugin enforces is the one the engine must enforce',
  );
});
