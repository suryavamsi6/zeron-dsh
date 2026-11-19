-- Settings → MCP servers (screens/settings/mcp): the MCP servers this
-- device runs beside the sessions' agents, each listed by name with its
-- command line, `env` by the variables' names only — a value never
-- reaching the tree. On the shell in ui/shell_fixture.luau, `McpServers`
-- answered by ui/settings_stub.luau (`settings_stub_mcp_servers`).

local PAGE = "#settings-page"
local NAV = "#settings-nav"

local function open(answer: any): any
	local app = mount("shell_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	if answer ~= nil then
		app:set_state("settings_stub_mcp_servers", answer)
	end
	app:command("settings_stub.sign_in")
	app:command("cloud_stub.offer")
	app:command("shell.settings")
	app:frames(10)
	-- (Under Zeron's heading, ahead of About: past the list's foot, so
	-- scrolled to first, as Plugins' are.)
	app:scroll(NAV, 400)
	app:frames(5)
	app:click(`{NAV} >> role:tab "MCP servers"`)
	app:frames(30)
	return app
end

local function calls(app: any): { string }
	return (app:state("settings_stub_calls") :: any) or {}
end

local function asked(app: any, entry: string): number
	local n = 0
	for _, c in calls(app) do
		if c == entry then
			n += 1
		end
	end
	return n
end

test("MCP servers lists each server's name and its command line", function()
	local app = open()
	expect(app:has(`{PAGE} >> "filesystem"`)).to_be(true)
	expect(app:has(`{PAGE} >> "github"`)).to_be(true)
	local tree = app:tree(PAGE)
	-- `command`, then each of `args`, a space between.
	expect(tree).to_contain("npx -y @modelcontextprotocol/server-filesystem /work/notes")
	expect(tree).to_contain("uvx mcp-server-github")
	-- Asked once, as the page opened.
	expect(asked(app, "McpServers")).to_be(1)
end)

test("MCP servers shows the variables' names, their values nowhere in the tree", function()
	local app = open()
	local tree = app:tree(PAGE)
	expect(tree).to_contain("NOTES_ROOT")
	expect(tree).to_contain("GITHUB_TOKEN")
	-- The values, nowhere in the page's tree.
	expect(tree).never.to_contain("filesystem-env-value-should-not-render")
	expect(tree).never.to_contain("github-token-value-should-not-render")
end)

test("MCP servers says when there are none", function()
	local app = open({ servers = {} })
	expect(app:has(`{PAGE} >> "No MCP servers yet"`)).to_be(true)
end)
