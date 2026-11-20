-- ui/mcp_live_fixture.luau: Settings → MCP servers with **no**
-- ui/settings_stub.luau, so every `ui.call` goes to the Rust host. `zgx
-- test ui` has no engine and installs no host actions (`zgx-script`'s
-- test_runner takes no host factory), so here the page must fail to list —
-- and must list none of the stub's servers. That is the one property this
-- runner can see, and the one that matters: a change that quietly requires
-- the stub again would show its two made-up servers instead of asking the
-- host. The second test pins the accessibility names Builder's harness
-- drives (the four points below).
--
-- The four real-engine assertions, for apps/zgx/src/mcp_live_tests.rs (the
-- bin unit test Builder adds with a `zeron-engine` dev-dependency: a
-- `TestApp` whose `McpServers`/`SetMcpServers` are the app's own actions
-- over `zeron_rpc::memory_client(EngineCore::rpc_service())` on a temp data
-- dir). They read `{data_dir}/mcp-servers.json`, and none of them runs
-- here:
--   1. add through the page → the server is in the engine's file;
--   2. a second add leaves the first's `env` byte-identical in the file;
--   3. removing the first leaves only the second, its `env` unchanged;
--   4. a refused save (17th server) shows the engine's words and leaves
--      the file untouched.
-- Unproven until that harness lands; the page's stub-backed coverage of the
-- same operations is tests/mcp_settings.test.luau.

local PAGE = "#settings-page"

test("MCP live fixture asks the Rust host, never the settings stub", function()
	local app = mount("mcp_live_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	-- No host action named `McpServers`: the page says so, as it does when
	-- the engine can't be reached.
	expect(app:has(`{PAGE} >> "Can't list MCP servers"`)).to_be(true)
	-- The stub's two servers, nowhere: the fixture did not fall back to it.
	expect(app:has(`{PAGE} >> role:button "Remove filesystem"`)).to_be(false)
	expect(app:has(`{PAGE} >> role:button "Remove github"`)).to_be(false)
	-- And adding stays off with no list to build a whole-list write on.
	expect(app:tree(`{PAGE} >> role:textbox "Name"`)).to_contain("disabled")
end)

test("MCP live fixture keeps the form and Save the harness drives", function()
	local app = mount("mcp_live_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	-- The names Builder's four-point test fills and clicks, on the failed
	-- list: present and readable, only disabled until there is a list.
	for _, field in { "Name", "Command", "Arguments", "Environment" } do
		expect(app:has(`{PAGE} >> role:textbox "{field}"`)).to_be(true)
	end
	expect(app:has(`{PAGE} >> role:button "Save"`)).to_be(true)
end)
