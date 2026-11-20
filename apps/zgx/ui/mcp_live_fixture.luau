-- Settings → MCP servers against the real engine: ui/screens/settings/mcp
-- with **no** ui/settings_stub.luau, so every host call is `ui.call` to the
-- Rust host — never a stand-in that answers in Luau. The Rust test that
-- mounts this (apps/zgx/src/mcp_live_tests.rs) installs `McpServers` and
-- `SetMcpServers` over an in-process `EngineCore` on a temp data dir, as
-- apps/zgx/src/providers.rs installs them in the app over the engine's IPC.
--
-- So `zgx test ui` cannot mount it: that runner has no engine and no Rust
-- host actions, and the page would say "Can't list MCP servers" (no host
-- action named `McpServers`). The page's headless coverage stays in
-- tests/mcp_settings.test.luau on ui/settings_stub.luau; this fixture is
-- the one the engine-backed test mounts. It is here, beside the other
-- fixtures, so the page's real host wiring is a mount away rather than a
-- second copy of the page.

ui.theme.strict("warn")
local ds = require("@ds")
ds.setup()
-- Addresses plain, as the shell fixture sets them: the test reads them.
ds.PrivateText.hidden:set(false)

local mcp = require("@app/screens/settings/mcp")

-- #settings-page where the shell's settings page would be: the page alone,
-- so the nav and the shell's own boot (and its stubs) stay out of it.
-- (Inside a component, as the shell's presence callback calls it: the
-- page keeps component state, its draft and its save.)
return ui.component(function(): Node
	return ui.div {
		id = "settings-page",
		w_full = true,
		h_full = true,
		mcp.view(),
	}
end)({})
