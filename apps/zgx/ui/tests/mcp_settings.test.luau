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

-- The whole lists `SetMcpServers` was asked to save, in order.
local function saved(app: any): { any }
	return (app:state("settings_stub_mcp_saves") :: any) or {}
end

-- The remove asking, by the server's name.
local function remove_dialog(name: string): string
	return `role:dialog "Remove “{name}”?"`
end

test("MCP servers refuses a name the engine would, without asking the host", function()
	local app = open()
	app:fill(`{PAGE} >> role:textbox "Name"`, "bad name!")
	app:fill(`{PAGE} >> role:textbox "Command"`, "npx")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(app:has(`{PAGE} >> "A name takes letters, digits, - and _ only."`)).to_be(true)
	expect(#saved(app)).to_be(0)
end)

test("MCP servers adds a server and sends the whole list, env values intact", function()
	local app = open()
	app:fill(`{PAGE} >> role:textbox "Name"`, "notes")
	app:fill(`{PAGE} >> role:textbox "Command"`, "npx")
	app:fill(`{PAGE} >> role:textbox "Arguments"`, "-y\n@modelcontextprotocol/server-notes")
	app:fill(`{PAGE} >> role:textbox "Environment"`, "NOTES_TOKEN=typed-token-value")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	local lists = saved(app)
	expect(#lists).to_be(1)
	expect(#lists[1]).to_be(3)
	local added = lists[1][3]
	expect(added.name).to_be("notes")
	expect(added.command).to_be("npx")
	expect(added.args[1]).to_be("-y")
	expect(added.args[2]).to_be("@modelcontextprotocol/server-notes")
	expect(added.env.NOTES_TOKEN).to_be("typed-token-value")
	-- The servers the person did not touch go back as they came, values included.
	expect(lists[1][1].env.NOTES_ROOT).to_be("filesystem-env-value-should-not-render")
	expect(lists[1][2].env.GITHUB_TOKEN).to_be("github-token-value-should-not-render")
	-- The list is asked again, so the rows are the engine's saved list.
	expect(asked(app, "McpServers")).to_be(2)
	expect(app:has(`{PAGE} >> "notes"`)).to_be(true)
end)

test("MCP servers asks before removing a server, naming it", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove filesystem"`)
	app:frames(10)
	local dialog = remove_dialog("filesystem")
	expect(app:has(dialog)).to_be(true)
	expect(app:has(`{dialog} >> "New sessions on this device won't load it."`)).to_be(true)
	expect(app:has(`{dialog} >> role:button "Remove server"`)).to_be(true)
	-- Asked about, not yet run.
	expect(#saved(app)).to_be(0)
end)

test("MCP servers cancels a remove on Escape, asking nothing of the host", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove filesystem"`)
	app:frames(10)
	app:key("Escape")
	app:frames(10)
	expect(#saved(app)).to_be(0)
	expect(asked(app, "SetMcpServers")).to_be(0)
	expect(asked(app, "McpServers")).to_be(1)
end)

test("MCP servers removes on Remove server and sends the list without it", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove filesystem"`)
	app:frames(10)
	app:click(`{remove_dialog("filesystem")} >> role:button "Remove server"`)
	app:frames(10)
	local lists = saved(app)
	expect(#lists).to_be(1)
	expect(#lists[1]).to_be(1)
	expect(lists[1][1].name).to_be("github")
	-- The server left goes back exactly as it came, env value intact.
	expect(lists[1][1].command).to_be("uvx")
	expect(lists[1][1].args[1]).to_be("mcp-server-github")
	expect(lists[1][1].env.GITHUB_TOKEN).to_be("github-token-value-should-not-render")
	expect(asked(app, "McpServers")).to_be(2)
end)

test("MCP servers keeps the draft when the host refuses the save", function()
	local app = open()
	app:set_state("settings_stub_mcp_save", `Two servers are named "notes"`)
	app:fill(`{PAGE} >> role:textbox "Name"`, "notes")
	app:fill(`{PAGE} >> role:textbox "Command"`, "npx")
	app:fill(`{PAGE} >> role:textbox "Environment"`, "NOTES_TOKEN=keep-this")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(app:tree(PAGE)).to_contain("Two servers are named")
	-- The draft is still there, to fix and send again.
	expect(app:text(`{PAGE} >> role:textbox "Name"`)).to_be("notes")
	expect(app:text(`{PAGE} >> role:textbox "Command"`)).to_be("npx")
	expect(app:text(`{PAGE} >> role:textbox "Environment"`)).to_be("NOTES_TOKEN=keep-this")
	-- One ask, refused: the draft is not resent behind the person's back.
	expect(asked(app, "SetMcpServers")).to_be(1)
end)
