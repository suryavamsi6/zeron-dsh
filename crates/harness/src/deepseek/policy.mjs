export const name = 'zeron-deepseek-policy';
export const inject = ['tools', 'approval', 'llm'];

const reads = new Set(['read', 'glob', 'grep', 'list', 'ask_user_question', 'captain_drive_read']);
const edits = new Set(['write', 'edit', 'apply_patch', 'captain_drive_write']);

export function assertRoute(options) {
  // The official route runs its two models and nothing else. Any other route
  // is one the host stamped (`ZERON_DEEPSEEK_PROVIDER`, which never rides the
  // wire, so a sender cannot choose it); DSH resolves that route's models and
  // reports an unknown one itself.
  if (options.provider === 'deepseek-official') {
    if (!['deepseek-flash', 'deepseek-v4-pro'].includes(options.model)) {
      throw new Error('Zeron only permits official DeepSeek model routes');
    }
    return;
  }
  if (typeof options.provider !== 'string' || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(options.provider)) {
    throw new Error('Zeron only permits official DeepSeek model routes');
  }
}


const workspaceEdits = new Set(['write', 'edit', 'apply_patch']);
const shells = new Set(['bash', 'persistent-bash', 'persistent-pwsh']);

function normalizeOwnedPath(path) {
  const text = String(path);
  return (process.platform === 'win32' ? text.replace(/\\/g, '/') : text).replace(/^\.\/+/, '');
}

function globRegex(glob) {
  const source = normalizeOwnedPath(glob);
  let out = '';
  for (let i = 0; i < source.length; i += 1) {
    const char = source[i];
    if (char === '*') {
      if (source[i + 1] === '*') {
        if (source[i + 2] === '/') {
          out += '(?:.*/)?';
          i += 2;
        } else {
          out += '.*';
          i += 1;
        }
      } else out += '[^/]*';
    } else if (char === '?') out += '[^/]';
    else out += char.replace(/[|\\{}()[\]^$+?.]/g, '\\$&');
  }
  return new RegExp(`^${out}$`);
}

function patchPaths(input) {
  if (typeof input !== 'string') return [];
  const paths = [];
  for (const line of input.split('\n')) {
    const file = line.match(/^\s*\*\*\* (?:Add|Update|Delete) File: (.+?)\s*$/)?.[1]
      ?? line.match(/^\s*\*\*\* Move to: (.+?)\s*$/)?.[1];
    if (file) paths.push(file.trim());
  }
  return paths;
}

function editPaths(exec) {
  const args = exec.arguments;
  if (typeof args !== 'object' || args === null) return [];
  if ((exec.name === 'write' || exec.name === 'edit') && typeof args.file_path === 'string') return [args.file_path];
  if (exec.name === 'apply_patch') return patchPaths(args.input);
  return [];
}

// SUR-206: a crew member's `owns` globs are a hard pre-dispatch gate. Paths
// are session-relative, matching the fs tools' own addressing; absolute or
// parent-traversing paths can never be made safe by comparing glob text, so
// they are outside every crew scope. The member's own report remains the one
// unconditional writeback.
export function ownsDenial(exec, captains) {
  if (captains?.role !== 'crew' || !Array.isArray(captains.owns) || captains.owns.length === 0) return undefined;
  if (shells.has(exec.name)) return `Crew member ${captains.chatId} may not use shell commands outside ownership-checked file tools`;
  if (!workspaceEdits.has(exec.name)) return undefined;
  const paths = editPaths(exec).map(normalizeOwnedPath);
  if (exec.name === 'apply_patch' && typeof exec.arguments?.input === 'string' && paths.length === 0)
    return 'Crew ownership cannot verify the files in this patch';
  if (paths.length === 0) return undefined;
  const report = `reports/${captains.chatId}.md`;
  const globs = captains.owns.filter(glob => typeof glob === 'string' && glob !== '').map(globRegex);
  const outside = paths.find(path => path.startsWith('/') || /^[A-Za-z]:\//.test(path) || path.split('/').includes('..')
    || (path !== report && !globs.some(glob => glob.test(path))));
  return outside === undefined ? undefined
    : `Crew member ${captains.chatId} may not edit ${outside}: outside its owns globs`;
}

export function requiresApproval(access, name) {
  if (!['supervised', 'auto-accept-edits', 'full-access'].includes(access)) {
    throw new Error('Unknown DeepSeek access mode');
  }
  return access !== 'full-access' && !reads.has(name)
    && !(access === 'auto-accept-edits' && edits.has(name));
}

export function apply(ctx, config) {
  requiresApproval(config.access, '');
  ctx.on('llm/stream', (options, next) => {
    assertRoute(options);
    return next();
  });
  const approved = new WeakSet();
  ctx.on('tools/pre-execute', async (exec, next) => {
    const decision = await next();
    if (decision.kind !== 'allow' || !requiresApproval(config.access, exec.name)) return decision;
    if (exec.signal.aborted) return { kind: 'cancel' };
    if (!exec.agent) return { kind: 'deny', reason: 'No agent owns this approval' };
    const outcome = await ctx.approval.request({
      agent: exec.agent, toolName: exec.name, callId: exec.callId,
      reason: `Zeron ${config.access}: approve ${exec.name}`,
      signal: exec.signal,
    });
    if (exec.signal.aborted || outcome === 'cancelled') return { kind: 'cancel' };
    if (outcome !== 'allowed-once') return { kind: 'deny', reason: 'Explicit tool approval was not granted' };
    approved.add(exec.token);
    return { kind: 'allow' };
  });
  ctx.tools.guard(exec => ownsDenial(exec, config.captains)
    ?? (requiresApproval(config.access, exec.name) && !approved.has(exec.token)
      ? 'Zeron requires explicit approval for this tool invocation' : undefined));
  ctx.provide('zeronPolicy', { ready: true });
}
