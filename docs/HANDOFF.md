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
- Linux system packages are installed with `scripts/ci/apt-install.sh`
  (`Acquire::Retries=3`, 15 s http timeout, `timeout 120` on `apt-get update`). Most apt
  failures are mirror flakes, so rerun before debugging.
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

### SUR-125: upstream parity is a repeating sync, not a one-off

`upstream` is `https://github.com/zeronsh/zeron`. After every merged PR, check drift with
`git fetch upstream && git rev-list --count origin/main..upstream/main`. Sync when **100+
commits behind**, or at every milestone boundary, whichever comes first; at **300+ behind**,
stop feature work and sync first. One branch `dsuryavamsi/sur-125-upstream-sync-<tag>`, one PR.
The sync PR is the **one type merged with a merge commit, not squash**: squashing would drop
upstream ancestry and every later sync would re-diff and re-conflict the same commits. Never
rebase or force-push `main`. Resolution rule: upstream wins for shared app behaviour; fork-only
work stays (dsh provider culling, zgx dsh pages, plugin runtime, docs); files we deliberately
deleted stay deleted. Gate: `CI result` green on the sync PR, release-profile failures triaged,
and `git merge-base --is-ancestor upstream/main main` true afterwards.

Round 1 is the current PR (#43): merge of upstream `e393c4a84` (0.3.9 + 169 commits) into
`main` `2bb0d54a`; it is a merge commit, not a squash.

### SUR-58: scheduled tasks are owned by the engine

dsh's schedule service lives in a long-lived Host process, but our sidecar is started once
per run, so the engine owns schedules. Follow-ups:
- SUR-116: engine store and runner (PR open)
- SUR-117: zgx Automation tasks page
- SUR-118: `schedule_*` tools exposed through an engine MCP in `extra_mcp`

### SUR-125: upstream sync cadence

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
- **Gate:** `CI result` green on the sync PR, release-profile failures triaged rather than re-run
  away, and `git merge-base --is-ancestor <round target> main` true afterwards.

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
