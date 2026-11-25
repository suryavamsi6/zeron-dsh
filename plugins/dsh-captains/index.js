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

// The head-commit pair below renders the step prompt with the runtime's own
// renderer and writes it back through the runtime's own message factory, so
// both come through the same peer anchors: an install where they cannot be
// reached leaves the row on the declare-only path rather than failing to load.
function peerService(specifier) {
  let resolved;
  return () => resolved ??= peerModule(specifier);
}

const dshLlm = peerService('@deepseek-ai/dsh-llm');
const dshSystemPrompt = peerService('@deepseek-ai/dsh-system-prompt');

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

function crewRuleBodies(text) {
  if (typeof text !== 'string') return [];
  const bodies = [];
  for (const match of text.matchAll(/<crew-rules(?:\s[^>]*)?>([\s\S]*?)<\/crew-rules>/g)) {
    let body = match[1];
    // crewRules() adds one structural newline around the file body.
    if (body.startsWith('\n') && body.endsWith('\n')) body = body.slice(1, -1);
    if (body !== '') bodies.push(body);
  }
  return bodies;
}

function messageText(content) {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .filter(block => block?.type === 'text' && typeof block.text === 'string')
    .map(block => block.text)
    .join('');
}

// SUR-85: a crew chat serves the Captain's CREW.md as a prompt section that
// re-reads the file at every assembly. The bytes served to each assembly's
// signal are remembered; when the next admitted first step of a turn sees the
// file has changed since the session's last admission, it declares a
// request-series rebase so the stale copy is rewritten rather than linger.
//
// SUR-255: the declaration lands on the decision AFTER the waterfall, so a
// listener inside that waterfall — pressure compaction, pricing the step and
// writing its checkpoint — still reads the retired head. A prepended pre-step
// listener therefore commits the rendered prompt over the live head BEFORE
// next(), when the live head differs from the captured assembly's rendered
// prompt. The admitted-step declaration below still tracks served crew bytes.
function applyCrew(ctx, cfg) {
  const registered = new WeakSet();
  const bytesBySignal = new WeakMap();
  const promptBySignal = new WeakMap();
  // In memory, keyed by session id. A missing entry means this process never
  // saw the bytes the resumed series was built from, so that first admitted
  // step must rebase; only a known equal baseline passes through untouched.
  const lastBytes = new Map();
  const repairChecked = new WeakSet();
  const repairCheckpoints = async (agent, signal) => {
    const { session } = agent;
    const current = crewRules(cfg);
    const [currentBody] = crewRuleBodies(current);
    if (currentBody === undefined) return;

    const retired = new Set();
    for (let seq = 1; seq <= session.seq; seq += 1) {
      const event = session.eventAt(seq);
      if (event?.type !== 'system/message') continue;
      const content = session.deriveEventMessage?.(event)?.content ?? event.data?.message?.content;
      for (const body of crewRuleBodies(messageText(content))) {
        if (body !== currentBody) retired.add(body);
      }
    }
    if (retired.size === 0) return;

    const contaminated = [];
    for (const seq of session.surface?.nodes ?? []) {
      const event = session.eventAt(seq);
      if (event?.type !== 'user/message' || event.data?.source?.kind !== 'compact-checkpoint') continue;
      const text = messageText(event.data?.content);
      if ([...retired].some(body => text.includes(body))) contaminated.push(seq);
    }
    if (contaminated.length === 0) return;

    for (const seq of contaminated) {
      const checkpoint = session.eventAt(seq);
      const compactionId = checkpoint?.data?.source?.compactionId;
      let summary;
      for (let at = seq - 1; at >= 0; at -= 1) {
        const event = session.eventAt(at);
        if (event?.type === 'compaction/summary' && event.data?.compactionId === compactionId) {
          summary = event;
          break;
        }
      }
      if (summary === undefined) throw new Error(`crew-rules checkpoint ${seq} has no paired compaction summary`);
      const cleanBlocks = summary.data.summary.map(block => {
        if (block?.type !== 'text' || typeof block.text !== 'string') return block;
        let text = block.text;
        for (const body of retired) text = text.replaceAll(body, '(none)');
        return { ...block, text };
      });

      // The service is optional and this row must load without it: use the
      // undeclared-access fallback when the direct read is refused.
      let compaction;
      try {
        compaction = ctx.compaction;
      } catch {
        compaction = undefined;
      }
      compaction ??= ctx.get?.('compaction');
      if (typeof compaction?.compactRegion !== 'function' || typeof compaction.summarize !== 'function') return;
      const originalSummarize = compaction.summarize;
      compaction.summarize = async (_input, owner, abort) => {
        abort?.throwIfAborted();
        const { provider, model } = session.requestHeader().config;
        return { summary: cleanBlocks, provider, model, maxTokens: compaction.config.maxTokens };
      };
      try {
        await compaction.compactRegion(seq, seq, agent, signal);
      } finally {
        compaction.summarize = originalSummarize;
      }
    }
  };
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
    // Capture the exact text the runtime will send for this step. Prepended
    // so `next()` returns the fully transformed presentation; keyed by the
    // turn signal, the same object the pre-step event carries.
    scope.on('system-prompt/assemble', async (_assembly, { signal }, next) => {
      const presentation = await next();
      const prompts = dshSystemPrompt();
      if (prompts !== undefined && signal != null) promptBySignal.set(signal, prompts.renderPrompt(presentation));
      return presentation;
    }, { prepend: true });
    scope.on('agent/pre-step', (event, next) => {
      const { agent, messages, signal, turn, step } = event;
      if (step !== 1 || !Array.isArray(messages) || messages.length === 0) return next();
      const { session } = agent;
      const headSeq = session.surface.nodes[0];
      const head = session.eventAt(headSeq);
      if (head?.type !== 'system/message') return next();
      const prompt = promptBySignal.get(signal);
      if (prompt === undefined) return next();
      const content = session.deriveEventMessage(head)?.content ?? [];
      const [block] = content;
      const headText = content.length === 0 ? ''
        : content.length === 1 && block?.type === 'text' && typeof block.text === 'string' ? block.text : undefined;
      // A multi-block head projects to undefined: the guard declines rather
      // than comparing what it cannot read.
      if (headText === undefined || headText === prompt) return next();
      const llm = dshLlm();
      if (llm === undefined) return next();
      signal.throwIfAborted();
      session.append('system/message', { turn, step, message: llm.createSystemMessage(prompt) },
        { surfaceOp: { op: 'replace', startSeq: headSeq, endSeq: headSeq }, sourceEventSeqs: [headSeq] });
      return next();
    }, { prepend: true });
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
    // SUR-263 clause 2: "retired" means the body of a crew-rules block this
    // session's own log served before the body CREW.md serves now; an archived
    // pre-fix head is what supplies that text. The WeakSet makes the check run
    // once per session, before the first request, and the repair finishes
    // before next() so the head commit, pricing, and automatic compaction
    // later in the same waterfall already see the clean surface.
    //
    // It must stay a compaction transaction. A `surfaceOp: replace` cannot
    // target the paired `compaction/summary` record because that record has no
    // surface node, and appending a clean `user/message` directly over the
    // checkpoint seq makes resumeSession reject the log (`compaction checkpoint
    // has no matching compaction/start`). compactRegion(seq, seq, ...) with a
    // call-local summarize override instead appends a matched start, summary,
    // checkpoint and end chain; the old events remain read-only history while
    // the new checkpoint shadows the contaminated one.
    scope.on('agent/pre-step', async ({ agent, signal }, next) => {
      const { session } = agent;
      if (session === null || typeof session !== 'object' || repairChecked.has(session)) return next();
      repairChecked.add(session);
      try {
        await repairCheckpoints(agent, signal);
      } catch (error) {
        ctx.logger?.info?.('zeron-captains: crew-rules checkpoint repair failed: %s',
          error instanceof Error ? error.message : String(error));
      }
      return next();
    }, { prepend: true });
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
