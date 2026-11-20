-- Settings → MCP servers for live-shots (apps/zgx/src/audit_shots.rs):
-- the shell (ui/shell_fixture.luau, as it is) opened on the servers page,
-- with the host's two stub servers (ui/settings_stub.luau), so the page that
-- adds and removes them can be captured as it draws:
--
--   AUDIT_SHOTS=settings_mcp@1240x900
--
-- (Signed in, as a device with an account is: the page sits under Zeron's
-- heading among the rest.)

local node = require("./shell_fixture")

local auth = require("@account/auth")
auth.account:set({
	state = "signed_in",
	env = "staging",
	user = { id = "user_ada", email = "ada@example.net", first_name = "Ada" },
	organization_id = "org_1",
} :: any)

require("@app/screens/shell/route").settings("mcp")

return node
