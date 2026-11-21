import { createRequire } from 'node:module';
import { realpathSync } from 'node:fs';
import { delimiter, join } from 'node:path';
import { captainPrompt } from './src/prompts.js';
import { driveToolSpecs } from './src/tools.js';
import { normalizeConfig } from './src/config.js';

export { normalizeConfig } from './src/config.js';

export const name = 'zeron-captains';
export const inject = ['systemPrompt', 'tools'];

// The dsh-tools peer is the loading runtime's to supply: a properly installed
// bundle resolves it under its own layout, and a linked checkout (`dsh plugin
// add <dir>`) leaves it to the runtime resolution, which routes it to the
// loading installation — outside a live runtime, the dsh package on PATH.
// The bundle still has to load where no copy is reachable: an inert row must
// not take the chat down with it.
function peerTools() {
  const anchors = [import.meta.url];
  for (const dir of (process.env.PATH ?? '').split(delimiter)) {
    for (const exe of ['dsh', 'dsh.exe', 'dsh.cmd']) {
      try { anchors.push(realpathSync(join(dir, exe))); } catch { /* no dsh on this PATH entry */ }
    }
  }
  anchors.push(join(process.cwd(), 'cordis.yml'));
  let missing;
  for (const anchor of anchors) {
    try {
      return createRequire(anchor)('@deepseek-ai/dsh-tools');
    } catch (error) {
      if (error?.code !== 'MODULE_NOT_FOUND' && error?.code !== 'ERR_REQUIRE_ESM') throw error;
      missing ??= error;
    }
  }
  return { defineTool: () => { throw missing; } };
}

const defineTool = spec => peerTools().defineTool(spec);

// SUR-81 creates the folder engine-side; the row's driveRoot is the Drive path here.
const driveDir = cfg => cfg.driveRoot;

export function apply(ctx, config) {
  const cfg = normalizeConfig(config);
  if (cfg.role === 'none') return;
  if (cfg.role === 'captain') {
    ctx.effect?.(() => ctx.systemPrompt.section({
      name: 'zeron-captains:role',
      order: ctx.systemPrompt.getSectionOrder('DEPLOYMENT_PERSONA_SUFFIX') - 1,
      text: () => captainPrompt(cfg, driveDir(cfg)),
      interpolate: false,
    }));
    for (const spec of driveToolSpecs(cfg)) ctx.tools.register(defineTool(spec));
  }
}
