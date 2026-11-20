-- Settings → MCP servers: the MCP servers this device runs beside the
-- sessions' agents (the engine's `McpServers`), each a program that adds
-- tools over the Model Context Protocol. The list is asked once as the
-- page opens (a `ui.resource`; Try again asks again); Add server takes a
-- name, a command, its arguments and its environment, and Save sends the
-- whole list through `SetMcpServers` — every server not being added or
-- removed goes back exactly as it came, `env` values included. A
-- server's row is its name and its command line under it in mono
-- (`command`, then its `args`, a space between) — and of `env`, only the
-- variables' names: their values never stand here, a token often being
-- one. Remove asks once and sends the shorter list.

local host = require("@app/screens/settings/host")
local ds = require("@ds")
local search = require("@app/screens/settings/search")

local M = { ID = "mcp", TITLE = "MCP servers" }
search.index(M.ID, "MCP servers", "mcp model context protocol servers tools command add remove", {
	{ key = "servers", title = "Servers", terms = "list command args env environment installed remove" },
	{ key = "add", title = "Add server", terms = "add new name command arguments environment" },
})

-- The engine's name rule (crates/engine/src/user_mcp.rs): letters, digits,
-- - and _ only, at most 32.
local NAME_MAX = 32

-- What went wrong asking the host, in words.
local function say(err: any): string
	return if type(err) == "table" and type(err.message) == "string" then err.message else tostring(err)
end

local function trim(s: string): string
	return (string.gsub(s, "^%s*(.-)%s*$", "%1"))
end

-- The name rule in the form's own words, or nil when the name fits.
local function name_problem(name: string): string?
	if name == "" then
		return "Give the server a name."
	end
	if #name > NAME_MAX then
		return `Names stop at {NAME_MAX} characters.`
	end
	if string.find(name, "[^A-Za-z0-9_%-]") then
		return "A name takes letters, digits, - and _ only."
	end
	return nil
end

-- A textarea's lines, one entry per line: what Arguments takes, and what
-- Environment's KEY=value lines are read through.
local function lines_of(text: string): { string }
	local out = {}
	for line in string.gmatch(text, "[^\r\n]+") do
		local trimmed = trim(line)
		if trimmed ~= "" then
			table.insert(out, trimmed)
		end
	end
	return out
end

-- The environment a textarea describes: each line KEY=value, the first =
-- splitting them, so a value may hold one. The value is kept as typed.
local function env_of(text: string): ({ [string]: string }?, string?)
	local env: { [string]: string } = {}
	for _, line in lines_of(text) do
		local key, value = string.match(line, "^([^=]-)=(.*)$")
		if key == nil then
			return nil, "Environment lines read KEY=value."
		end
		key = trim(key)
		if key == "" then
			return nil, "Environment lines need a key before the =."
		end
		env[key] = value
	end
	return env, nil
end

-- A server's command line as it would be typed: `command`, then each of
-- `args`, a space between.
local function command_line(s: any): string
	if type(s) ~= "table" then
		return ""
	end
	local parts = {}
	if type(s.command) == "string" and s.command ~= "" then
		table.insert(parts, s.command)
	end
	if type(s.args) == "table" then
		for _, a in s.args do
			if type(a) == "string" then
				table.insert(parts, a)
			end
		end
	end
	return table.concat(parts, " ")
end

-- The variables a server sets, by name only (sorted, so the line reads
-- the same every draw): the values never leave this string.
local function env_names(s: any): string
	if type(s) ~= "table" or type(s.env) ~= "table" then
		return ""
	end
	local names = {}
	for k, _ in s.env do
		if type(k) == "string" then
			table.insert(names, k)
		end
	end
	table.sort(names)
	return table.concat(names, ", ")
end

-- A server's row: its name, and under it — where a row's own words go —
-- its command line in the mono face, a long one fading at its end, then
-- the variables it sets, by name only. Remove asks once, on Remove
-- server, then the page sends the list without it.
local Row = ui.component(function(props: {
	now: State<any>,
	busy: State<boolean>,
	on_remove: (name: string) -> (),
}): Node
	local now = props.now
	local function name(): string
		local s = now:get()
		return if type(s) == "table" and type(s.name) == "string" then s.name else ""
	end
	return ds.Settings.Row {
		label = name,
		stacked = true,
		control = ui.row {
			w_full = true,
			gap = "$space.3",
			items = "center",
			ui.column {
				grow = 1,
				min_w = 0,
				gap = "$zeron.space.hair",
				ds.FadeText {
					function(): string
						return command_line(now:get())
					end,
					literal = true,
					font = "$font.mono",
					text_size = "$font.size.sm",
					line_height = "$font.line.sm",
					color = "$text.tertiary",
					min_w = 0,
				},
				ui.show(function(): boolean
					return env_names(now:get()) ~= ""
				end, ui.text {
					function(): string
						return `env: {env_names(now:get())}`
					end,
					literal = true,
					font = "$font.mono",
					text_size = "$font.size.xs",
					line_height = "$font.line.xs",
					color = "$text.tertiary",
					wrap = true,
				}),
			},
			ds.IconButton {
				icon = "trash-bin-minimalistic",
				label = function(): string
					return `Remove {name()}`
				end,
				size = "sm",
				tone = "danger",
				disabled = props.busy,
				on_click = function()
					if name() ~= "" then
						props.on_remove(name())
					end
				end,
			},
		},
	}
end)

function M.view(): Node
	-- Asked as the page opens; Try again asks again, and a landed write
	-- asks it once more so the rows are the engine's list.
	local res = ui.resource(function(): any
		return host.call("McpServers", {}):await()
	end)
	local servers = ui.computed(function(): { any }
		local d = res.data:get()
		return if type(d) == "table" and type(d.servers) == "table" then d.servers else {}
	end)
	-- The add form's draft, kept on screen when the host refuses it.
	local draft_name = ui.use_state("")
	local draft_command = ui.use_state("")
	local draft_args = ui.use_state("")
	local draft_env = ui.use_state("")
	-- A write (add or remove) on its way; the row's Remove stays off.
	local saving = ui.use_state(false)
	local problem = ui.use_state(nil :: string?)
	-- The server whose remove is being asked about.
	local removing = ui.use_state(nil :: string?)
	local remove_open = ui.use_state(false)
	local function kind(): string
		if res.data:get() ~= nil then
			return if #servers:get() > 0 then "list" else "empty"
		end
		return if res.status:get() == "failed" then "failed" else "loading"
	end
	-- A write is off until the list is known: Save sends the whole list, so
	-- with none loaded it would send only the new server and drop the rest.
	local function locked(): boolean
		return saving:get() or res.data:get() == nil
	end
	-- The name as it is typed: said wrong only once there is something to
	-- be wrong about — the engine's rule, or a name this list already has
	-- (the engine would refuse that too; the person can see why before Save).
	local function typed_name_problem(): string?
		local n = trim(draft_name:get())
		if n == "" then
			return nil
		end
		local bad = name_problem(n)
		if bad then
			return bad
		end
		for _, s in servers:get() do
			if type(s) == "table" and s.name == n then
				return `A server named “{n}” is already in this list.`
			end
		end
		return nil
	end
	-- Send the whole list: every entry not being added or removed is the
	-- table the host answered with, so its `env` values go back intact.
	local function write(next_servers: { any }, landed: (() -> ())?)
		if saving:peek() then
			return
		end
		saving:set(true)
		problem:set(nil)
		host.call("SetMcpServers", { servers = next_servers }):after(function(answer: any)
			saving:set(false)
			if type(answer) == "table" and type(answer.servers) == "table" then
				-- The engine's answer is the list now: keep it as the page's
				-- data, so a write before the reload lands builds on it, and a
				-- reload that fails leaves it rather than the list before
				-- this save.
				res.data:set(answer)
				if landed then
					landed()
				end
				res.reload()
			else
				problem:set("The host answered without the servers it saved.")
			end
		end, function(err: any)
			saving:set(false)
			problem:set(say(err))
		end)
	end
	local function save()
		if saving:peek() then
			return
		end
		if res.data:peek() == nil then
			problem:set("The server list hasn't loaded. Retry above, then add.")
			return
		end
		local n = trim(draft_name:get())
		local bad = typed_name_problem()
		if n == "" then
			bad = "Give the server a name."
		end
		if bad then
			problem:set(bad)
			return
		end
		local cmd = trim(draft_command:get())
		if cmd == "" then
			problem:set("Give the server a command to run.")
			return
		end
		local env, env_bad = env_of(draft_env:get())
		if env == nil then
			problem:set(env_bad or "Environment lines read KEY=value.")
			return
		end
		local next_servers: { any } = {}
		for _, s in servers:get() do
			table.insert(next_servers, s)
		end
		table.insert(next_servers, {
			name = n,
			command = cmd,
			args = lines_of(draft_args:get()),
			env = env,
		})
		write(next_servers, function()
			draft_name:set("")
			draft_command:set("")
			draft_args:set("")
			draft_env:set("")
		end)
	end
	local function ask_remove(name: string)
		if saving:peek() then
			return
		end
		removing:set(name)
		remove_open:set(true)
	end
	local function confirm_remove()
		local victim = removing:peek()
		if victim == nil then
			return
		end
		local next_servers: { any } = {}
		for _, s in servers:get() do
			if type(s) ~= "table" or s.name ~= victim then
				table.insert(next_servers, s)
			end
		end
		removing:set(nil)
		write(next_servers)
	end
	return ds.Settings.Page {
		title = "MCP servers",
		description = "The MCP servers your agents on this device can use.",
		search.anchor("servers", ds.Settings.Section {
			ui.presence(kind, function(k: string): Node
				if k == "loading" then
					-- (Said, not only drawn: the bars are hidden from assistive
					-- technology.)
					return ui.column {
						w_full = true,
						label = ui.t("common.loading"),
						live = "polite",
						ds.Settings.Group {
							ds.SkeletonRow { title = 130, line = 180, trail = 90 },
							ds.SkeletonRow { title = 100, line = 200, trail = 90 },
						},
					}
				elseif k == "failed" then
					return ds.EmptyState {
						icon = "remote-server",
						size = "sm",
						title = "Can't list MCP servers",
						body = function(): string
							local err = res.error:get()
							return if err then `{say(err)} Try again in a moment.` else "Try again in a moment."
						end,
						action = { label = "Retry", on_click = res.reload },
					}
				elseif k == "empty" then
					return ds.EmptyState {
						icon = "remote-server",
						size = "sm",
						title = "No MCP servers yet",
						body = "MCP servers give your agents extra tools — a filesystem, a GitHub client, a database — over the Model Context Protocol.",
					}
				end
				return ds.Settings.Each(servers, function(_s: any, _i: number, now: State<any>): Node
					return Row { now = now, busy = saving, on_remove = ask_remove }
				end, {
					key = function(s: any): string
						return s.name
					end,
					wash = true,
				})
			end, { w_full = true, enter = { opacity = { 0, 1 }, duration = 120 }, exit = { opacity = 0, duration = 60 }, mode = "wait" }),
		}),
		search.anchor("add", ds.Settings.Section {
			title = "Add server",
			description = "A program this device starts beside your agents, over the Model Context Protocol.",
			ds.Settings.Row {
				label = "Name",
				stacked = true,
				control = ui.column {
					w_full = true,
					gap = "$space.1",
					ds.TextField {
						value = draft_name,
						name = "Name",
						placeholder = "filesystem",
						size = "sm",
						w_full = true,
						error = typed_name_problem,
						disabled = locked,
					},
				},
			},
			ds.Settings.Row {
				label = "Command",
				stacked = true,
				control = ds.TextField {
					value = draft_command,
					name = "Command",
					placeholder = "npx",
					size = "sm",
					w_full = true,
					disabled = locked,
				},
			},
			ds.Settings.Row {
				label = "Arguments",
				description = "One per line.",
				stacked = true,
				control = ds.TextArea {
					value = draft_args,
					name = "Arguments",
					placeholder = "-y\n@modelcontextprotocol/server-filesystem\n/work/notes",
					rows = 2,
					max_rows = 6,
					size = "sm",
					mono = true,
					w_full = true,
					disabled = locked,
				},
			},
			ds.Settings.Row {
				label = "Environment",
				description = "KEY=value, one per line. Values stay on this device.",
				stacked = true,
				control = ds.TextArea {
					value = draft_env,
					name = "Environment",
					placeholder = "NOTES_ROOT=/work/notes",
					rows = 2,
					max_rows = 6,
					size = "sm",
					mono = true,
					w_full = true,
					disabled = locked,
				},
			},
			ui.row {
				w_full = true,
				gap = "$space.3",
				items = "center",
				ui.show(function(): boolean
					return res.data:get() == nil
				end, ds.Hint {
					grow = 1,
					function(): string
						return if res.status:get() == "failed"
							then "The list didn't load, so adding is off. Retry above."
							else "The list is still loading."
					end,
				}),
				ui.show(function(): boolean
					return problem:get() ~= nil
				end, ds.Hint {
					tone = "danger",
					grow = 1,
					function(): string
						return problem:get() or ""
					end,
				}),
				ds.Button {
					"Save",
					size = "sm",
					busy = saving,
					disabled = locked,
					on_click = save,
				},
			},
		}),
		-- (The asking stands over the page wherever it is built.)
		ds.Dialog {
			open = remove_open,
			title = (function(): string
				return `Remove “{removing:get() or ""}”?`
			end) :: any,
			body = "New sessions on this device won't load it.",
			tone = "danger",
			size = "sm",
			confirm = {
				label = "Remove server",
				on_click = confirm_remove,
			},
		},
	}
end

return M
