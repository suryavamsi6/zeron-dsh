# zeron-dsh handoff (to Hermes)

Written 2026-11-18. Source of truth for scope is the Linear project
**zeron-dsh roadmap** (team SUR). Work order: M0 → M1 → SUR-78 gate → M2 → M3 →
Captains (SUR-97, plugin-first).

## Workflow that was used

- One issue per branch `dsuryavamsi/sur-<n>-<slug>`, one git worktree each, cut from `origin/main`.
- All product code was written by a coding agent from a short brief; the operator only
  reviewed, committed, opened PRs, and watched CI.
- PR title ends with `(SUR-<n>)`. Merge (squash) when the **`CI result`** check is green.
- After merging, add a close comment on the Linear issue: what changed, the PR link, the merge
  commit, and any follow-ups.
- `gh` was always called with `-R suryavamsi6/zeron-dsh`. Never force-push `main`.

## Merged PRs

| PR | Issue | What |
|----|-------|------|
| #15 | SUR-23 | harness: dsh plugin bundle contract test |
| #16 | SUR-19 | zgx: host actions for dsh plugins |
| #17 | SUR-48 | harness: dsh web_search/web_fetch as WebSearch/WebFetch cards |
| #18 | SUR-110 | harness+engine: needsClient flag for browser-only client modules |
| #19 | SUR-64 | sidecar: skip non-image attachments instead of failing the turn |
| #20 | SUR-111 | harness: per-chat Web search switch for DeepSeek runs |
| #21 | SUR-63 | zgx: list a web search's sources under its card |
| #22 | SUR-20 | zgx: Settings → DeepSeek plugins page |
| #23 | SUR-68 | harness: DeepSeek session titles via tool-less sdk-minimal run |
| #24 | SUR-65 | zgx: warn and hold sends when V4 Pro gets an image |
| #25 | SUR-24 | docs(harness): dsh plugins section + DeepSeek titles line |
| #26 | SUR-29 | ci: dsh plugin bundle contract test in deepseek-compatibility |
| #27 | SUR-112 | harness: read a DeepSeek chat export into plain conversations |
| #28 | SUR-113 | engine: ReadAloud RPC (DeepSeek TTS) |
| #29 | SUR-21 | zgx: install a dsh plugin from a spec |

## Open PRs / branches at handoff

| PR | Issue | Branch | State at handoff |
|----|-------|--------|------------------|
| #30 | SUR-115 | `dsuryavamsi/sur-115-zgx-docs-localize-the-v4-pro-image` | Ready. Every check passed except `zgx-linux`, which was still pending. Merge when `CI result` is green. |
| #31 | SUR-116 | `dsuryavamsi/sur-116-engine-scheduled-tasks-store-and-runner` | Ready. Code is complete: 4 unit tests and `cargo check -p zeron-rpc` pass locally. CI just started. |
| #32 | SUR-22 | `dsuryavamsi/sur-22-zgx-remove-a-dsh-plugin-with-confirmation` | Ready. Code is complete and the 12 dsh plugin tests pass, but the full UI suite run was cut off. Merged with main. CI just started. |
| #33 | SUR-114 | `dsuryavamsi/sur-114-zgx-read-aloud-message-action-plays-a` | **Draft (WIP).** The code is in, but the full UI suite and the Rust build haven't been confirmed. The new deps in `apps/zgx/Cargo.toml` need review. |
| this PR | — | `handoff/hermes` | This document. |

## Next-up order

1. Land the open PRs above (SUR-115, SUR-116, SUR-22; finish SUR-114).
2. SUR-25 → SUR-26 → SUR-28. All of these touch `apps/zgx/ui/screens/settings/dsh_plugins.luau`, so
   do them **one at a time** and don't run them in parallel.
3. **SUR-78 gate** (M1 exit). Don't start M2 until it passes.
4. M2/M3: SUR-117 (zgx Automation tasks page) and SUR-118 (schedule_* tools through an engine
   MCP in `extra_mcp`), both after SUR-116. Then SUR-27.
5. Captains: start with the SUR-97 real-build probe, then follow the plugin-first split below.

## CI notes

- Jobs: `plan`, `admit`, `linux-core`, `zgx-linux` (20–40 min), `zgx-macos`,
  `deepseek-compatibility`, `ios`, plus the aggregate **`CI result`**. The aggregate is the
  only required check, so merge when it is green. If one job fails on a known infra flake,
  rerun the failed jobs once before you look into it.
- **DeepSeek runtime gate:** `deepseek-compatibility` runs the ignored
  `sdk_runtime_round_trip_and_resume` after installing the pinned SDK, so a dependency bump
  that disagrees with the sidecar's plan-mode start fails there instead of waiting for a
  manual run. It is loopback-only (a local mock model), so it needs no network or key. To
  run it by hand: `ZERON_DEEPSEEK_SDK_DIR=crates/harness/src/deepseek cargo nextest run
  --run-ignored ignored-only --locked -p zeron-harness -E
  'test(sdk_runtime_round_trip_and_resume)'`.
- Linux system packages are installed with `scripts/ci/apt-install.sh`
  (`Acquire::Retries=3`, 15 s http timeout, `timeout 120` on `apt-get update`). Most apt
  failures are mirror flakes, so rerun before debugging.
- **Windows `ort-sys` (SUR-129):** the crate caches its extracted DirectML runtime in a
  runner-owned directory and skips the download whenever that directory merely exists, so a stale
  or partial cache makes `copy_dylibs` copy a DLL that was never written and the app build fails at
  `dynamic_link.rs:42` (`Os { code: 3, NotFound }`). The Windows app build therefore starts from a
  job-local, emptied `ORT_CACHE_DIR` (`${{ runner.temp }}\ort-cache`) and deletes
  `apps\zgx\target\release\build\ort-sys-*` after `rust-cache` restores `target`: those restored
  `OUT_DIR`s hold `DirectML.dll` links into the cache that was just emptied.
- **zeron-v2 template rule:** if you change a default zgx file that has a copy under
  `apps/zgx/ui/templates/zeron-v2/` (listed in that template's `.base.json`), you must port the
  change into the copy **and** update its hash in `.base.json`. The hash is the Rust
  `DefaultHasher` over the LF-normalized bytes, formatted `{:016x}`. Otherwise
  `zeron_v2_is_in_step_with_the_default` fails.
- **rustfmt:** never run `rustfmt`/`cargo fmt` on `lib.rs`, `rpc.rs` or `main.rs`. It recurses into
  submodules and reformats dozens of files that aren't rustfmt-clean today. To format new
  files only, use `rustfmt --config skip_children=true <file>`.
- `apps/zgx/ui` tests: the expected failures are in `scripts/ci/zgx-known-failures.txt`.

### Known flakes

- `chat_persistence::a_kill_mid_save_loses_and_doubles_nothing` (linux)
- engine `plugins::service_tests::a_window_calls_its_service_here` (macOS)
- `device_stats_of_an_offline_device_say_so_within_the_deadline` (seen once on main)
- In the full local zgx UI suite, a few tests flake on timing: locale, session-tab,
  route-history, "a settings link (About's rows)", and files_fixture mount-time. They aren't
  listed in `zgx-known-failures.txt` and passed in CI.

## Decisions

### SUR-58: scheduled tasks are owned by the engine

dsh's schedule service lives in a long-lived Host process, but our sidecar is started once
per run, so the engine owns schedules. Follow-ups:
- SUR-116: engine store and runner (PR open)
- SUR-117: zgx Automation tasks page
- SUR-118: `schedule_*` tools exposed through an engine MCP in `extra_mcp`

### SUR-125: upstream parity is a repeating sync

`zeron-dsh` is a fork of `zeronsh/zeron`; parity is kept by merging `upstream/main` in,
never by rebasing.

- **Check:** after every merged PR, `git fetch upstream && git rev-list --count origin/main..upstream/main`.
  Record the literal count and the time it was measured in `/workspace/CHECKPOINT.md`; never carry a
  previous value forward. Scribe owns this check.
- **Sync when** 100+ commits behind, or at a milestone boundary, whichever comes first.
  300+ behind: stop feature work and sync first.
- **How:** one branch `dsuryavamsi/sur-125-upstream-sync-<tag>`, `upstream/main` merged in,
  conflicts resolved, one PR.
- **Merge commit, never squash** — the single exception to the squash rule. A squash drops upstream
  ancestry, so the next sync re-diffs the same commits and re-conflicts on the same files.
- **No rebase or force-push of `main`:** the fork commits are published and PR-linked, and rewriting
  them breaks those references.
- **Resolutions:** upstream wins for shared app behaviour; fork-only work stays (the dsh provider
  cull, zgx dsh pages, plugin runtime, docs). Files deliberately deleted stay deleted.
- **Round boundary:** a round targets the upstream tag it started against. Upstream keeps moving
  while a sync is in review, so commits that arrive mid-round are the *next* round rather than an
  extension of the current one. Round 1 closed at 0.3.10 (`d6fad8238`) in PR #43; `17d777fa0`
  (RollText, Grok approval) is round 2.
- **Rounds so far:** round 2 at **0.3.12** (`2e3513c67`) merged as `0779d8cd6` (PR #66); round 3
  at **0.3.13** (`13cf7db91`) merged as `bf9fd5913` (PR #77). Both kept `upstream/main` an
  ancestor: drift 0 behind after each.
- **Round 3 (0.3.13) — what was not taken, and why:**
  - **Code Mode** is `zeron_harness::supports_code_mode`-gated for **Codex and Muse only**, both
    cut. Its auto-merged hunks were reverted on top of the merge: `RunRequest.code_mode`,
    `CodeModeSettings`/`code_mode()`, the `GetCodeMode`/`SetCodeMode` RPCs, the warm-start
    replacement and every test's `RunRequest` initializer. No generic fix rode along: the only
    non-Code-Mode hunk in those files is `crates/engine/tests/warm_start.rs` adding Codex coverage,
    which is cut-provider, and `crates/engine/src/harness_updates.rs` / `crates/harness/README.md`
    are Antigravity docs and updaters.
  - **ACP registry agents** (`register_registry_agent`, `descriptors_for(include_registry)`) not
    resurrected — they live in `acp_registry.rs`, which the cut removed.
  - **Fork state kept** in `screens/settings/providers.luau`, `harness_updates.rs`, `registry.rs`,
    `sessions.rs` (schedule bridge and user-MCP blocks), and the three Claude-only engine tests.
  - **49 `modify/delete` conflicts** kept the deletion (46 from the DeepSeek-only cut `f9a17677a`,
    3 from earlier cut-preserving merges).
  - **`HarnessId::LEGACY` (11 entries) stays** for wire decode of old sessions; it is not a
    resurrected provider, and `HarnessId::ALL` remains exactly `[DeepSeek, Mock]`.
  - Locale strings for the cut provider pages remain in `en.json`/`zh-Hans.json` as inert data.
  - Templates: `zeron-v2/.base.json` regenerated 8/8 from `apps/zgx/ui/<rel>` after the merge.
- **Gate:** `CI result` green on the sync PR, release-profile failures triaged rather than re-run
  away, and `git merge-base --is-ancestor <round target> main` true afterwards.

#### Round 2 — 2026-11-20, upstream 0.3.11 (`33d981bd3`) and 0.3.12 (`2e3513c67`)

- **Range merged:** `d6fad8238..33d981bd3` (5 commits): `22eb085c5` 0.3.11, `8c5549ecb` Cloud:
  every built-in agent with every login, `17d777fa0` Grok Auto + Full access always-approve,
  `4820dd83c` RollText interface font, `33d981bd3` the release merge. 64 conflicted paths against
  `origin/main` `80c72f7a1`. Merge commit, never squash.
- **Upstream moved mid-round, so the tip was taken too:** `33d981bd3..2e3513c67` (3 commits:
  `890c4527e` Antigravity 1.5.0 hardening + provider-switch seam, `5ac8d75a5` 0.3.12,
  `2e3513c67` the release merge) merged on top as a second merge commit, plus current `main`
  (`432a0724b`) in between. That is what keeps `upstream/main` an ancestor — the acceptance is
  `git merge-base --is-ancestor upstream/main main`, not the release the round started against.
- **Resolutions worth repeating:**
  - 27 `modify/delete` paths were all the provider cut: keep deleted. 0.3.12's ACP work
    (`acp/mod.rs`, `acp_stall.rs`, `acp.rs`, `provider_switch.rs`, new `ccpa_thoughts.rs` and
    `antigravity_live.rs`) is the same cut and stays out.
  - The fork's rewritten modules (`agent_accounts`, `cloud_accounts`, `credentials`,
    `harness_updates`, `install`, `plan_usage`, `providers`, `registry`) take the fork's version
    wholesale; upstream's edits inside them are provider machinery.
  - The 0.3.12 queued-message config seam is kept in `rpc.rs` *behind* the fork's
    `ensure_chat_harness` guard; `sessions.rs` takes upstream's provider-switch seam and
    recovery-history carry, because the surrounding merged code is upstream's.
  - `runtime_survival`'s held-send test now asserts upstream's Stop contract: the send stays in
    the paused queue and runs on the person's next message, rather than being cancelled into the
    transcript.
  - Generic upstream fixes kept: `known_connections` (a client drops vault providers it does not
    know, so an edge newer than the client listing one must not fail the whole status read),
    `newest_node_dir` / `prefer_node_dir` on a Cloud machine (with `executable::node_version`
    ported, since `adapter_install` is cut), `starts_with` (a new chat shows the agent it was
    sent with at once), `steers` / `turn_running` in the composer, and the sur-75 MCP page.
  - **Vault status provider filter: not taken, deliberately.** Upstream 0.3.12's edge filters a
    status's `connections` to the providers the client names in `VAULT_PROVIDERS_HEADER`
    (`understoodProviders` / `forClient` in `edge/src/vault-routes.ts`). This tree defines that
    header (`crates/proto/src/cloud.rs`) and sends it (`crates/client/src/cloud.rs`), but its
    edge does not filter. Safe today: the vault can only ever emit `deepseek-key` and `github`
    (`VAULT_PROVIDERS` in `edge/vault/src/api.ts`, `isProvider`, `VaultProvider::ALL`), and the
    client already drops unknown names (`known_connections`). The one break path is future: add
    a third vault provider and serve a client released before this merge, which has neither the
    header nor the client-side filter. Before that happens port `understoodProviders` /
    `forClient` with a legacy default of today's two providers.
  - **Deliberately not taken:** upstream's multi-provider cloud-agent feature —
    `model_publish`, `sign_in_catalog` + `engine discover-models`, the per-provider vault
    materials, the expanded `CLOUD_HARNESSES`, provider UI in `agents.rs` and the iOS agent
    list, and the 17-provider vault enum/`ALL`. The cut keeps DeepSeek and the GitHub key only;
    Cloud accounts stay the fork's `deepseek-key` implementation. The Edge publish half —
    `POST /runner/models`, `POST /cloud/{orgId}/models` and the `Published*` types in
    `edge/src/cloud/models.ts` — stays as forward-compat for that future producer; the read half
    (`GET /cloud/{orgId}/models` via `cloudModels(published.latest)`) is live picker surface today.
  - **Taken from it:** `grantMany` and `POST /vault/{org}/grants` — the route is back at
    `edge/src/vault-routes.ts:250` with its batch test re-pointed to `deepseek-key`; the batched
    RPC is provider-agnostic, so it does not need the multi-provider catalog.
  - `UserInputQuestion::note` is new: every in-tree constructor needs `note: None` (`deepseek`
    was the only one that did not already carry it).
  - zeron-v2 template: the default `composer.luau` moved with the Cloud commit and both copies
    took it; `.base.json` re-blessed from this tree's default — composer `c3aecf97b99d7bfc`
    (the fork's own delta), lane_tab `5958161a55b26ba4` (identical to upstream's).
- **Gate run on the merged tree:** `cargo nextest run -p zeron-engine -p zeron-harness` and
  `cargo check --locked --all-targets` for `zeron-proto`, `zeron-harness`, `zeron-engine`,
  `zeron-mcp`, `zeron-client` and `zeron`.

### SUR-97: Captains on dsh (desk findings, dsh 0.2.2-rc.3)

| | Capability | dsh support |
|---|---|---|
| a | Prompt sections | `ctx.systemPrompt.section` / `ctx.systemPrompt.context` |
| b | Tools | `ctx.tools.register`; `policy.mjs` can require approval |
| c | Lifecycle hooks | `agent/created`, `agent/status`, `agent/turn-stopping`, `tools/pre-execute`, `tools/post-execute` and `turn/end`. There is **no** awaiting-input event. |
| d | Cross-chat control | The sdk profile has no cross-chat session controller, so creating and messaging crew members stays in the Zeron MCP/engine. |
| e | Plugin files | `node:fs`, `dsh-home-paths` and `ctx.storageDomain` |
| f | Skills and commands | `ctx.skills.register` and `ctx.commands.register` |
| g | Packaging | `dsh.bundle.patch` + `dsh plugin --profile sdk add`. The invocation patch can override row config per chat. |

These findings still need a probe against a real build, once SUR-78 passes.

### Captains: plugin-first split

- **Plugin `plugins/dsh-captains/`:**
  - role prompt and CREW.md rules
  - Drive starter files and Drive tools
  - digest skill
  - `needs-you.json`
  - crew report
- **Engine gaps (separate issues):**
  - crew registry and its RPCs
  - stamping the role into the plugin row
  - policy allowlist for the Drive tools
  - installing the bundle
  - crew auto-join
  - cross-process reports
  - notifications
