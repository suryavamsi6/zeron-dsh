import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { escapeAttr } from './prompts.js';

const CREW_CAP = 8 * 1024;
const TRUNCATION_NOTE = '\n[CREW.md truncated at 8 KB]';

// The cap counts UTF-8 bytes, so a cut has to be byte-honest: a multibyte
// codepoint is kept whole or dropped whole, never split into a mojibake edge.
export function cutUtf8(text, cap = CREW_CAP) {
  let bytes = 0;
  let end = 0;
  for (const ch of text) {
    const n = Buffer.byteLength(ch, 'utf8');
    if (bytes + n > cap) break;
    bytes += n;
    end += ch.length;
  }
  return text.slice(0, end);
}

// The section's text callback is synchronous, so the file read is too; it is
// re-read at every assembly and a missing or blank file contributes nothing.
export function crewRules(cfg) {
  let content;
  try {
    content = readFileSync(join(cfg.driveRoot, 'CREW.md'), 'utf8');
  } catch {
    return '';
  }
  if (content.trim() === '') return '';
  const body = Buffer.byteLength(content, 'utf8') > CREW_CAP
    ? cutUtf8(content) + TRUNCATION_NOTE
    : content;
  return `<crew-rules captain="${escapeAttr(cfg.captainName)}">\n${body}\n</crew-rules>\n`;
}
