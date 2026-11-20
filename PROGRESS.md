# SUR-145 — the MCP page against a real engine (UI half)

Branch `dsuryavamsi/sur-145-test-the-mcp-page-against-a-real-engine`, cut from
`origin/main` `a18dab1a` (squash of #68). Authority: push the branch only — no
PR, no merge. Parent work: SUR-75's add/remove page, SUR-72/73's engine side.

## What changed

- `apps/zgx/ui/mcp_live_fixture.luau` (new) — mounts
  `apps/zgx/ui/screens/settings/mcp.luau` inside `#settings-page` with **no**
  `ui/settings_stub.luau`, inside a component (`mcp.luau` uses
  `ui.use_state`, which errors at top level). No shell, nav, auth or cloud
  boot. Every `ui.call` goes to the Rust host; Builder's harness installs
  `McpServers`/`SetMcpServers` over an in-process `EngineCore`.
- `apps/zgx/ui/tests/mcp_live.test.luau` (new) — two tests: the fixture asks
  the Rust host and lists none of the stub's servers (so it can't quietly
  fall back to `settings_stub`), and the form/Save names Builder's four-point
  test drives are present. The four real-engine assertions are written there
  as the contract (file `{data_dir}/mcp-servers.json`); they are **unproven**
  until `apps/zgx/src/mcp_live_tests.rs` lands.
- `apps/zgx/ui/screens/settings/mcp.luau` — `say()` only: strips a leading
  `bad params: ` / `failed: ` / `capability error: ` from the engine's `err`
  string (the `RpcError` Display the app passes through), so a refused save
  reads as the engine's sentence.
- `apps/zgx/ui/tests/mcp_settings.test.luau` — the stub-backed test for that
  strip: three refusals, sentence shown, envelope word nowhere. Written red
  first: `ui/tests/mcp_settings.test.luau:226: expected false to be true`,
  then green after the `say()` change.

## Verified

- `zgx test ui "without the RPC envelope"` — red before (`0 passed, 1
  failed`, line 226), green after (`1 passed, 0 failed`).
- `zgx test ui "MCP"` — **18 passed, 0 failed** (15 on `a18dab1a` before this
  branch).
- `zgx lint` on the four Lua files — **0 errors**, 1 warning
  (`$zeron.space.hair` at `mcp.luau:154`), pre-existing: the diff touches
  only the `say()` block at lines 27-42.
- Full `zgx test ui` — **1498 passed, 0 failed** (15→18 for the MCP tests on
  this branch: +1 guard, +1 form/names, +1 envelope strip).

## Assumption (recorded, not asked)

The four-point page test cannot be a Luau test: `zgx-script`'s
`test_runner::run(dir, filter, each)` (crates/zgx-script/src/test_runner.rs:78)
takes no host factory and the runner installs no host actions, so no Luau
test can answer `McpServers` from an engine. The four assertions therefore
live as the documented contract in `mcp_live.test.luau`, run by Builder's bin
unit test; the two runnable Luau tests guard the fixture and the names.

## Not done

- No PR, no merge (authority: branch only).
- The four real-engine assertions: unproven until Builder's harness.
- `apps/zgx/src/**`, `crates/**`, `docs/**` untouched.
