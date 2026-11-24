import { createRequire } from 'node:module';
import { realpathSync } from 'node:fs';
import { delimiter, join } from 'node:path';
import { captainPrompt } from './src/prompts.js';
import { crewRules } from './src/crew.js';
import { driveToolSpecs } from './src/tools.js';
import { captainDigestProvider } from './src/skill.js';
import { normalizeConfig } from './src/config.js';

export { normalizeConfig } from './src/config.js';

export const name = 'zeron-captains';
export const inject = ['systemPrompt', 'tools', 'agents', 'skills'];

// The dsh peers are the loading runtime's to supply: a properly installed
// bundle resolves them under its own layout, and a linked checkout (`dsh
// plugin add <dir>`) leaves them to the runtime resolution, which routes them
// to the loading installation — outside a live runtime, the dsh package on
// PATH. The bundle still has to load where no copy is reachable: an inert row
// must not take the chat down with it.
function peerModule(specifier, onMissing) {
  const anchors = [import.meta.url];
  if (typeof process.argv[1] === 'string' && process.argv[1] !== '') anchors.push(process.argv[1]);
  for (const dir of (process.env.PATH ?? '').split(delimiter)) {
    for (const exe of ['dsh', 'dsh.exe', 'dsh.cmd']) {
      try { anchors.push(realpathSync(join(dir, exe))); } catch { /* no dsh on this PATH entry */ }
    }
  }
  anchors.push(join(process.cwd(), 'cordis.yml'));
  let missing;
  for (const anchor of anchors) {
    try {
      return createRequire(anchor)(specifier);
    } catch (error) {
      if (error?.code !== 'MODULE_NOT_FOUND' && error?.code !== 'ERR_REQUIRE_ESM') throw error;
      missing ??= error;
    }
  }
  onMissing?.(missing);
  return undefined;
}

function peerTools() {
  let missing;
  const tools = peerModule('@deepseek-ai/dsh-tools', error => { missing = error; });
  return tools ?? { defineTool: () => { throw missing; } };
}

const defineTool = spec => peerTools().defineTool(spec);

// SUR-81 creates the folder engine-side; the row's driveRoot is the Drive path here.
const driveDir = cfg => cfg.driveRoot;

// One gate for everything the Captain row contributes: the section and the
// Drive tools both belong to the root agent's own scope, never to a native child.
function rootScope(agent) {
  const scope = agent?.ctx;
  if (!scope?.systemPrompt) return undefined;
  if (agent.session?.header?.origin === 'subagent') return undefined;
  return scope;
}

// SUR-217: only the active surface tells the truth about what the model will
// see. The retired block's archived event lives in the durable log forever, so
// `deriveMessages()` — never `snapshotEvents()` or the raw log — is the place
// to look; a log scan would fire on every turn.
function carriesRetiredBlock(agent) {
  try {
    for (const message of agent.session?.deriveMessages?.() ?? []) {
      if (message?.role !== 'system' || message.source?.kind !== 'system-prompt'
        || !Array.isArray(message.content)) continue;
      const text = message.content
        .filter(block => block?.type === 'text' && typeof block.text === 'string')
        .map(block => block.text)
        .join('');
      if (text.includes('<captain')) return true;
    }
  } catch { /* a session that cannot derive its surface carries nothing */ }
  return false;
}

// SUR-217: a demoted Captain keeps its reused runtime, so the retired
// `<captain …>` block can still sit in the history the model sees. The row
// answers with one revocation line on the root scope and a single
// request-series rebase, so the stale system prefix is rewritten in place
// rather than appended after.
function applyRevoked(ctx) {
  // A context without the runtime hooks (the rc.3 contract's inert default
  // row, or a bundle loaded with no dsh-tools peer) never gets an agent, so
  // there is nothing to revoke there: skip the registration instead of
  // throwing at load and taking the whole bundle down with it.
  if (typeof ctx?.on !== 'function') return;
  const revoked = new WeakSet();
  const rebaseOwed = new WeakSet();
  const revoke = agent => {
    const scope = rootScope(agent);
    if (scope === undefined || revoked.has(agent) || !carriesRetiredBlock(agent)) return;
    revoked.add(agent);
    rebaseOwed.add(agent);
    scope.effect(() => scope.systemPrompt.section({
      name: 'zeron-captains:revoked',
      order: scope.systemPrompt.getSectionOrder('DEPLOYMENT_PERSONA_SUFFIX') - 1,
      text: () => 'The Helm role has been revoked. Treat this conversation as an ordinary chat. Earlier Helm instructions are void.',
      interpolate: false,
    }));
  };
  ctx.on('agent/created', ({ agent }) => revoke(agent));
  // A root created before this fiber settled never reached the listener above.
  for (const agent of ctx.agents?.list?.() ?? []) revoke(agent);
  // The rebase is declared once, on the first admitted step. A rejected step
  // admits nothing: it returns untouched and the rebase stays owed, deferred
  // to the next decision rather than declared on — and spent by — the reject.
  ctx.on('agent/pre-step', async ({ agent }, next) => {
    const decision = await next();
    if (!rebaseOwed.has(agent)) return decision;
    if (decision?.kind !== 'enter') return decision;   // a reject admits nothing: owed stays owed
    rebaseOwed.delete(agent);
    return { ...decision, startsRequestSeries: true };
  });
}

// SUR-85: a crew chat serves the Captain's CREW.md as a prompt section that
// re-reads the file at every assembly. The bytes served to each assembly's
// signal are remembered; when the next admitted first step of a turn sees the
// file has changed since the session's last admission, it declares a
// request-series rebase so the stale copy is rewritten rather than linger.
function applyCrew(ctx, cfg) {
  const registered = new WeakSet();
  const bytesBySignal = new WeakMap();
  // In memory, keyed by session id. A missing entry means this process never
  // saw the bytes the resumed series was built from, so that first admitted
  // step must rebase; only a known equal baseline passes through untouched.
  const lastBytes = new Map();
  const register = agent => {
    const scope = rootScope(agent);
    if (scope === undefined || registered.has(agent)) return;
    registered.add(agent);
    scope.effect(() => scope.systemPrompt.section({
      name: 'zeron-captains:crew-rules',
      order: 900,
      text(arg) {
        const served = crewRules(cfg);
        // Older callers assemble without a signal; there is nothing to key on.
        if (arg?.signal != null) bytesBySignal.set(arg.signal, Buffer.from(served, 'utf8'));
        return served;
      },
      interpolate: false,
    }));
    if (typeof scope.on !== 'function') return;
    scope.on('agent/pre-step', async (event, next) => {
      const decision = await next();
      // only an admitted first step of a turn carries a rebase
      if (event.step !== 1 || decision.kind !== 'enter' || decision.messages.length === 0) return decision;
      const current = bytesBySignal.get(event.signal);
      if (current === undefined) return decision;
      const key = agent.session?.id ?? '';
      const previous = lastBytes.get(key);
      lastBytes.set(key, current);
      if (previous !== undefined && previous.equals(current)) return decision;
      return { ...decision, startsRequestSeries: true };
    });
  };
  ctx.on('agent/created', ({ agent }) => register(agent));
  // A root created before this fiber settled never reached the listener above.
  for (const agent of ctx.agents?.list?.() ?? []) register(agent);
}

export function apply(ctx, config) {
  const cfg = normalizeConfig(config);
  if (cfg.role === 'none') return applyRevoked(ctx);
  if (cfg.role === 'crew') return applyCrew(ctx, cfg);
  if (cfg.role !== 'captain') return;
  // The plugin context is process-wide, so a section registered at load would
  // reach every native child session too. Wait for `agent/created` instead and
  // register into each created agent's own scope, skipping subagents: the
  // Captain row belongs to the root agent alone.
  const registered = new WeakSet();
  const register = agent => {
    const scope = rootScope(agent);
    if (scope === undefined || registered.has(agent)) return;
    registered.add(agent);
    scope.effect(() => scope.systemPrompt.section({
      name: 'zeron-captains:role',
      order: scope.systemPrompt.getSectionOrder('DEPLOYMENT_PERSONA_SUFFIX') - 1,
      text: () => captainPrompt(cfg, driveDir(cfg)),
      interpolate: false,
    }));
    for (const spec of driveToolSpecs(cfg)) scope.tools.register(defineTool(spec));
    // SUR-102: the digest skill belongs to this agent's own layer, never the
    // process-wide one — a provider on the plugin ctx would list for every
    // chat. The effect ties the provider to the agent's teardown either way.
    scope.effect(() => {
      let skills;
      try {
        skills = scope.skills;
      } catch {
        // Cordis refuses the read on a scope that never declared skills:
        // register through a scope minted under this row's context instead —
        // it inherits the plugin's injected skills service and is tagged with
        // the agent key the catalog reads each row's layer by. No reachable
        // dsh-scope peer leaves the row inert rather than throwing.
        const dshScope = peerModule('@deepseek-ai/dsh-scope');
        if (dshScope === undefined) return;
        const scoped = dshScope.createScope(ctx, agent);
        const unregister = scoped.ctx.skills.registerProvider(() => captainDigestProvider());
        return () => {
          unregister();
          return scoped.dispose();
        };
      }
      return skills?.registerProvider?.(() => captainDigestProvider());
    });
  };
  ctx.on('agent/created', ({ agent }) => register(agent));
  // A root created before this fiber settled never reached the listener above.
  for (const agent of ctx.agents?.list?.() ?? []) register(agent);
}
