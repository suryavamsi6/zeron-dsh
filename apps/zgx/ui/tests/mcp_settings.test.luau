-- Settings → MCP servers (screens/settings/mcp): the MCP servers this
-- device runs beside the sessions' agents, each listed by name with its
-- command line, `env` by the variables' names only — a value never
-- reaching the tree; adding one, and removing one after asking, the whole
-- list sent through `SetMcpServers` with its untouched entries as they
-- came. On the shell in ui/shell_fixture.luau, `McpServers` and
-- `SetMcpServers` answered by ui/settings_stub.luau
-- (`settings_stub_mcp_servers`, `settings_stub_mcp_set`; the lists sent
-- are in `settings_stub_mcp_calls`).

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

-- The `SetMcpServers` writes the stub heard (`settings_stub_mcp_calls`),
-- the whole list sent each time.
local function writes(app: any): { any }
	return (app:state("settings_stub_mcp_calls") :: any) or {}
end

local function fill_server(app: any, name: string, command: string, args: string, env: string)
	app:fill(`{PAGE} >> role:textbox "Name"`, name)
	app:fill(`{PAGE} >> role:textbox "Command"`, command)
	app:fill(`{PAGE} >> role:textbox "Arguments"`, args)
	app:fill(`{PAGE} >> role:textbox "Environment"`, env)
end

test("MCP servers refuses a name that breaks the engine's rule before asking the host", function()
	local app = open()
	fill_server(app, "my server", "npx", "", "")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(app:has(`{PAGE} >> "A name takes letters, digits, - and _ only."`)).to_be(true)
	expect(#writes(app)).to_be(0)
	-- The draft stays for the person to fix.
	expect(app:text(`{PAGE} >> role:textbox "Name"`)).to_be("my server")
end)

test("MCP servers says a name is needed before asking the host", function()
	local app = open()
	fill_server(app, "", "npx", "", "")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(app:has(`{PAGE} >> "Give the server a name."`)).to_be(true)
	expect(#writes(app)).to_be(0)
end)

test("MCP servers refuses a name the list already has, without asking the host", function()
	local app = open()
	fill_server(app, "github", "uvx", "", "")
	expect(app:has(`{PAGE} >> "A server named “github” is already in this list."`)).to_be(true)
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(#writes(app)).to_be(0)
end)

test("MCP servers refuses an environment line that is not KEY=value", function()
	local app = open()
	fill_server(app, "notes", "npx", "", "JUST_A_KEY")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(app:has(`{PAGE} >> "Environment lines read KEY=value."`)).to_be(true)
	expect(#writes(app)).to_be(0)
end)

test("MCP servers keeps adding off when the list failed to load", function()
	local app = open("the engine is asleep")
	expect(app:has(`{PAGE} >> "Can't list MCP servers"`)).to_be(true)
	expect(app:has(`{PAGE} >> "The list didn't load, so adding is off. Retry above."`)).to_be(true)
	expect(app:tree(`{PAGE} >> role:button "Save"`)).to_contain("disabled")
	fill_server(app, "notes", "npx", "", "")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(#writes(app)).to_be(0)
end)

test("MCP servers builds the next write on the saved answer when the reload fails", function()
	local app = open()
	-- The reload after the save fails: the write landed, the read didn't.
	app:set_state("settings_stub_mcp_reload_fails", true)
	fill_server(app, "notes", "npx", "", "")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(20)
	-- Saved: its row is listed even though the reload failed.
	expect(app:has(`{PAGE} >> role:button "Remove notes"`)).to_be(true)
	-- And the next write builds on the engine's answer, not the list before
	-- the save: four servers, not three.
	fill_server(app, "second", "uvx", "", "")
	app:scroll(PAGE, 1000)
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(20)
	local w = writes(app)
	expect(#w).to_be(2)
	local servers = w[2].servers
	expect(#servers).to_be(4)
	expect(servers[1].name).to_be("filesystem")
	expect(servers[2].name).to_be("github")
	expect(servers[3].name).to_be("notes")
	expect(servers[4].name).to_be("second")
end)

test("MCP servers saves a new server, sending the whole list with untouched entries as they came", function()
	local app = open()
	fill_server(app, "notes", "npx", "-y\nnotes-server\n/work/notes", "NOTES_TOKEN=notes-token-value")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(20)
	local w = writes(app)
	expect(#w).to_be(1)
	local servers = w[1].servers
	expect(#servers).to_be(3)
	-- The two that were already there, the tables the page read.
	expect(servers[1].name).to_be("filesystem")
	expect(servers[1].command).to_be("npx")
	expect(#servers[1].args).to_be(3)
	expect(servers[1].args[1]).to_be("-y")
	expect(servers[1].args[2]).to_be("@modelcontextprotocol/server-filesystem")
	expect(servers[1].args[3]).to_be("/work/notes")
	expect(servers[1].env.NOTES_ROOT).to_be("filesystem-env-value-should-not-render")
	expect(servers[2].name).to_be("github")
	expect(servers[2].command).to_be("uvx")
	expect(servers[2].env.GITHUB_TOKEN).to_be("github-token-value-should-not-render")
	-- The new one, as typed: Arguments one per line, Environment KEY=value.
	expect(servers[3].name).to_be("notes")
	expect(servers[3].command).to_be("npx")
	expect(#servers[3].args).to_be(3)
	expect(servers[3].args[1]).to_be("-y")
	expect(servers[3].args[2]).to_be("notes-server")
	expect(servers[3].args[3]).to_be("/work/notes")
	expect(servers[3].env.NOTES_TOKEN).to_be("notes-token-value")
	-- Saved: the row is listed (its variables by name only), the form is
	-- empty, and the list is asked again.
	expect(app:has(`{PAGE} >> role:button "Remove notes"`)).to_be(true)
	expect(app:tree(PAGE)).never.to_contain("notes-token-value")
	expect(app:text(`{PAGE} >> role:textbox "Name"`)).to_be("")
	expect(asked(app, "McpServers")).to_be(2)
end)

test("MCP servers keeps the draft and shows the engine's words when a save is refused", function()
	local app = open()
	app:set_state("settings_stub_mcp_set", "A server named duplicate already exists.")
	fill_server(app, "duplicate", "npx", "", "")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(20)
	expect(app:has(`{PAGE} >> "A server named duplicate already exists."`)).to_be(true)
	expect(app:text(`{PAGE} >> role:textbox "Name"`)).to_be("duplicate")
	-- Not reloaded: the list on screen is still the one that was there.
	expect(asked(app, "McpServers")).to_be(1)
end)

-- A refusal reaches the page as the engine's `err` string, whose Display is
-- `crates/rpc`'s `RpcError` — it carries the RPC taxonomy ahead of the
-- sentence (`crates/rpc/src/server.rs`'s `respond` sends `err.to_string()`).
-- The person reads the sentence alone.
test("MCP servers shows the engine's words without the RPC envelope", function()
	local app = open()
	local refusals = {
		{ prefix = "bad params: ", said = "At most 16 MCP servers" },
		{ prefix = "capability error: ", said = "mcp_servers_unavailable" },
		{ prefix = "transport: ", said = "the engine went away" },
		{ prefix = "unknown method: ", said = "SetMcpServers" },
	}
	for _, refusal in refusals do
		app:set_state("settings_stub_mcp_set", refusal.prefix .. refusal.said)
		fill_server(app, "notes", "npx", "", "")
		app:click(`{PAGE} >> role:button "Save"`)
		app:frames(10)
		expect(app:has(`{PAGE} >> "{refusal.said}"`)).to_be(true)
		-- The envelope's word, nowhere.
		expect(app:tree(PAGE)).never.to_contain(refusal.prefix)
	end
	-- A bare failure has no envelope (`RpcError::Failed` is `{0}`): the
	-- sentence must reach the page whole, not with a word cut off it.
	app:set_state("settings_stub_mcp_set", "Could not save the MCP servers: disk full")
	fill_server(app, "notes", "npx", "", "")
	app:click(`{PAGE} >> role:button "Save"`)
	app:frames(10)
	expect(app:has(`{PAGE} >> "Could not save the MCP servers: disk full"`)).to_be(true)
end)

-- The load path takes the same `say()`: a list that fails on a transport
-- error, or against an engine that does not serve the method, must read as
-- the sentence too.
test("MCP servers shows the engine's words without the envelope when the list fails", function()
	for _, failure in {
		{ prefix = "transport: ", said = "the engine went away" },
		{ prefix = "unknown method: ", said = "McpServers" },
	} do
		local app = open(failure.prefix .. failure.said)
		expect(app:has(`{PAGE} >> "Can't list MCP servers"`)).to_be(true)
		expect(app:tree(PAGE)).to_contain(failure.said)
		expect(app:tree(PAGE)).never.to_contain(failure.prefix)
	end
end)

test("MCP servers asks before removing a server, naming it", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove filesystem"`)
	app:frames(30)
	local dialog = `role:dialog "Remove “filesystem”?"`
	expect(app:has(dialog)).to_be(true)
	expect(app:has(`{dialog} >> "New sessions on this device won't load it."`)).to_be(true)
	expect(app:has(`{dialog} >> role:button "Cancel"`)).to_be(true)
	expect(app:has(`{dialog} >> role:button "Remove server"`)).to_be(true)
	-- Asked about, not yet written.
	expect(#writes(app)).to_be(0)
end)

test("MCP servers cancels a remove without asking the host anything", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove filesystem"`)
	app:frames(30)
	app:click(`role:dialog "Remove “filesystem”?" >> role:button "Cancel"`)
	app:frames(10)
	expect(#writes(app)).to_be(0)
	expect(app:has(`{PAGE} >> role:button "Remove filesystem"`)).to_be(true)
end)

test("MCP servers removes on Remove server, sending the rest byte-for-byte", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove filesystem"`)
	app:frames(30)
	app:click(`role:dialog "Remove “filesystem”?" >> role:button "Remove server"`)
	app:frames(20)
	local w = writes(app)
	expect(#w).to_be(1)
	local servers = w[1].servers
	expect(#servers).to_be(1)
	expect(servers[1].name).to_be("github")
	expect(servers[1].command).to_be("uvx")
	expect(#servers[1].args).to_be(1)
	expect(servers[1].args[1]).to_be("mcp-server-github")
	expect(servers[1].env.GITHUB_TOKEN).to_be("github-token-value-should-not-render")
	-- Reloaded: the removed row gone, the other still there.
	expect(app:has(`{PAGE} >> role:button "Remove filesystem"`)).to_be(false)
	expect(app:has(`{PAGE} >> role:button "Remove github"`)).to_be(true)
end)

-- The fixture live-shots mount (ui/settings_mcp_fixture.luau): the shell
-- opened on this page, so `AUDIT_SHOTS=settings_mcp` draws what a person
-- sees without a click list.
test("MCP servers' live-shot fixture opens the page itself", function()
	local app = mount("settings_mcp_fixture.luau")
	app:resize(1240, 900)
	app:frames(30)
	expect(app:has(`{NAV} >> role:tab "MCP servers"`)).to_be(true)
	expect(app:has(`{PAGE} >> role:button "Remove filesystem"`)).to_be(true)
	expect(app:has(`{PAGE} >> role:textbox "Name"`)).to_be(true)
	expect(app:has(`{PAGE} >> role:button "Save"`)).to_be(true)
end)
