-- Settings → MCP servers, for live shots (src/audit_shots.rs): the main
-- window as ui/shell_fixture.luau mounts it, signed in the way the stub's
-- `settings_stub.sign_in` signs in, and opened on the MCP page, with
-- ui/settings_stub.luau answering `McpServers` / `SetMcpServers` (the two
-- servers, the list a save lands, a refusal). The page is the app's own:
-- a shot's `^…` steps open the add form or a row's remove asking.

local node = require("./shell_fixture")

-- (`settings_stub.sign_in`, which a shot has no command step to run before
-- the page mounts.)
require("@account/auth").account:set({
	state = "signed_in",
	env = "staging",
	user = { id = "user_ada", email = "ada@example.net", first_name = "Ada" },
	organization_id = "org_1",
} :: any)

require("@app/screens/shell/route").settings("mcp")

return node
