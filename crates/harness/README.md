# DeepSeek Harness

`deepseek::DeepSeekHarness` drives [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
(`dsh`) through its **official subprocess SDK**, not over direct model HTTP:

- [`@deepseek-ai/dsh-sdk-client`](https://www.npmjs.com/package/@deepseek-ai/dsh-sdk-client) —
  pinned, spawns the same-version `@deepseek-ai/dsh` runtime as `dsh --profile sdk` and
  speaks newline-delimited JSON-RPC 2.0 over stdio.
- [`@deepseek-ai/dsh-sdk-protocol`](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/sdk/protocol/README.md) —
  the wire contract: `initialize`, `session/prompt`, `session/query`, `session/resume`,
  `session/wait`, working-directory methods, `shutdown`, plus `session.event`,
  `session.status`, `approval/request`, and `user-question/ask`.

`mod.rs` owns the Rust `Harness` implementation; `shim.mjs` is the Node sidecar that
loads the published SDK and forwards its notifications as JSONL frames to Rust. Rust
never speaks model HTTP and never re-implements the JSON-RPC frame parser.

## Install (manual, explicit)

Installs are user actions; `install::can_install(DeepSeek)` is `false` and the manual
command is the pinned npm line. Node **>= 24.9** is required. Use an isolated prefix:

```sh
export ZERON_DEEPSEEK_SDK_DIR="$HOME/.local/share/zeron/deepseek-sdk/0.2.2-rc.3"
npm install --prefix "$ZERON_DEEPSEEK_SDK_DIR" --save-exact \
  @deepseek-ai/dsh-sdk-client@0.2.2-rc.3 @deepseek-ai/dsh@0.2.2-rc.3
```

`ZERON_DEEPSEEK_SDK_DIR` selects that prefix; the default is
`<data dir>/zeron/deepseek-sdk/<pin>`. `new()`/`discover()` only stat the manifest and
bin entries at the exact pin — they never execute the SDK, probe versions, or download.

## Authentication and homes

- Only `DEEPSEEK_API_KEY` (and an optional `DEEPSEEK_BASE_URL`) is forwarded, from the
  provider settings or the launching environment. No other provider credential is
  inherited, and there is no provider fallback: a missing key or an unavailable model
  fails the run instead of switching providers.
- The harness home is explicit and app-managed: the host stamps `ZERON_DEEPSEEK_HOME`
  (serde-skipped, never on the wire) per profile; otherwise
  `<data dir>/zeron/deepseek`. `~/.dsh` is never discovered or reused.
- The child's credential store is a per-run temporary `.credentials.yaml` inside the
  harness home, so a key stored for another product cannot be read.

## Route enforcement

Every model request — the SDK-created root agent and its in-process descendants — passes
the official `llm/stream` waterfall (`packages/llm/src/index.ts`). The sidecar's policy
plugin rejects any provider other than `deepseek-official` and any model other than the
two advertised ids before adapter I/O, so a subagent cannot select another provider.
The published default catalog is `deepseek-flash` (default, text + image) and
`deepseek-v4-pro`; the catalog is advertised statically (`sdk-bundled`) because those
ids are a verified constant of the pinned SDK, not a network probe.

## Permissions, questions, and tools

The required `sdk-jsonrpc-server` injects `zeronPolicy`, so a runtime that cannot load
the policy fails startup instead of running permissively. The policy maps Zeron modes:

| Mode | Reads | Edits | Commands / MCP / unknown tools |
| --- | --- | --- | --- |
| Supervised | allow | ask | ask |
| Auto-accept edits | allow | allow | ask |
| Full access | allow | allow | allow |

Asks use the SDK's `approval/request` (a grant is `allowed-once` only; a dismissal, an
error, or a cancelled turn is a denial), and a monotonic `ctx.tools.guard` re-checks that
each execution was approved, so listener order can never turn a refusal into permission.
Native `ask_user_question` calls map to `user-question/ask` (every question id answered,
or `{ cancelled: true }`). Tool subprocesses keep the SDK sandbox and the SDK's child
environment: it scrubs names matching `KEY|PASSWORD|SECRET|TOKEN`, so a token in the
runtime's own environment does not reach an agent's `bash`. Git identity for commits
comes from the engine's git configuration files, not from a token in the child env.

MCP servers from `RunRequest::mcp`/`extra_mcp` are composed as
`@deepseek-ai/dsh-mcp-client` stdio entries (`failOnStartupError: true`). Only those
supplied by the host are mounted; no ambient MCP configuration is inherited.

## dsh plugins

The `sdk` profile is a plain pnpm project at `<harness home>/profiles/sdk`: its
`dependencies` are the installed bundles and `dsh.profile.bundles` names the ones
dsh loads, so listing (Settings → DeepSeek plugins) reads two manifests per
bundle — no pnpm, no dsh, no writes. A bundle is reported `needsClient` when its
own manifest, or a package its patch file inserts, declares `dsh.client`: some
part of it renders in a browser, which zgx cannot host.

Adds and removes run dsh's own `plugin` subcommand —
`dsh plugin --profile sdk add|remove <spec>` on the pinned runtime — one at a
time, since the profile is a single `node_modules`. They need pnpm 10 or newer
on PATH (the spec is validated before any process exists), and a bundle whose
`engines.dsh` refuses the bundled version reports the refusal. Changes land on
sessions begun after; the agent's own `tool-plugin-manager` stays patched off,
so a session cannot change the profile it runs on. dsh's
[publishing guide](https://github.com/deepseek-ai/deepseek-harness/blob/39163dd30f703a79079e2d2446545cff3c453738/docs/user/develop/basic/publish.md)
and
[plugin-management reference](https://github.com/deepseek-ai/deepseek-harness/blob/39163dd30f703a79079e2d2446545cff3c453738/apps/cli/reference/README.md#plugin-management)
cover the bundle format and the subcommand.

## Sessions, interruption, and steering

- `RunRequest::resume` resumes through `session/resume`; a fresh run mints a session id
  and the SDK persists history under the harness home.
- The SDK protocol has **no cancel method**. Interruption closes the owned runtime
  (`shutdown`, then the SDK's own dispose ladder) and reports `Done { Interrupted }`; the
  next message resumes the persisted session.
- Steering is turn-boundary: a prompt that arrives while a turn runs is queued and sent
  after `Done`, reported as `Steered`. Launch-affecting configuration changes (model,
  effort, access, sandbox, cwd, options, MCP) are rejected rather than silently changing
  a live runtime.
- Streaming is as durable as the SDK allows: `session.event` notifications carry committed
  events. Assistant text and reasoning are replayed from the persisted
  `assistant/message` stream records in order (not live token deltas); tool calls,
  results, subagent traffic, usage, and turn ends are normalized from the same events.
- Titles run on the standalone `sdk-minimal` profile, not `sdk`: `run_title`
  re-points the start config (`title`) so the shim's whole patch is the tool
  switches that disable every agent tool the minimal profile carries, with the
  shared title instructions as its system prompt and a 32-token cap. A title
  run takes no MCP servers, no resume, and no reasoning effort.

## Verification

The contract suite runs the **actual published SDK** against a fake `dsh` stdio peer and,
separately, boots the real runtime against a loopback-only mock model (no live key, no
external network):

```sh
ZERON_DEEPSEEK_SDK_DIR=<pinned npm prefix> \
  node --test crates/harness/tests/deepseek_sdk.test.mjs
```

The ignored Rust round trip uses the same prefix and fixture:

```sh
ZERON_DEEPSEEK_SDK_DIR=<pinned npm prefix> \
  cargo test -p zeron-harness deepseek::tests::sdk_runtime_round_trip_and_resume -- --ignored
```
