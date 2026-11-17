export const name = 'zeron-deepseek-policy';
export const inject = ['tools', 'approval', 'llm'];

const reads = new Set(['read', 'glob', 'grep', 'list', 'ask_user_question']);
const edits = new Set(['write', 'edit', 'apply_patch']);

export function assertRoute(options) {
  if (options.provider !== 'deepseek-official' || !['deepseek-flash', 'deepseek-v4-pro'].includes(options.model)) {
    throw new Error('Zeron only permits official DeepSeek model routes');
  }
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
  ctx.tools.guard(exec => requiresApproval(config.access, exec.name) && !approved.has(exec.token)
    ? 'Zeron requires explicit approval for this tool invocation' : undefined);
  ctx.provide('zeronPolicy', { ready: true });
}
