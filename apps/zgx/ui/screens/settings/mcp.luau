-- Settings → MCP servers: the MCP servers this device runs beside the
-- sessions' agents (the engine's `McpServers`), each a program that adds
-- tools over the Model Context Protocol. Read-only — the list is asked
-- once as the page opens (a `ui.resource`; Try again asks again). A
-- server's row is its name and its command line under it in mono
-- (`command`, then its `args`, a space between) — and of `env`, only the
-- variables' names: their values never stand here, a token often being
-- one.

local host = require("@app/screens/settings/host")
local ds = require("@ds")
local search = require("@app/screens/settings/search")

local M = { ID = "mcp", TITLE = "MCP servers" }
search.index(M.ID, "MCP servers", "mcp model context protocol servers tools command", {
	{ key = "servers", title = "Servers", terms = "list command args env environment installed" },
})

-- What went wrong asking the host, in words.
local function say(err: any): string
	return if type(err) == "table" and type(err.message) == "string" then err.message else tostring(err)
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
-- the variables it sets, by name only.
local Row = ui.component(function(props: { now: State<any> }): Node
	local now = props.now
	return ds.Settings.Row {
		label = function(): string
			local s = now:get()
			return if type(s) == "table" and type(s.name) == "string" then s.name else ""
		end,
		stacked = true,
		control = ui.column {
			w_full = true,
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
	}
end)

function M.view(): Node
	-- Asked as the page opens; Try again asks again.
	local res = ui.resource(function(): any
		return host.call("McpServers", {}):await()
	end)
	local servers = ui.computed(function(): { any }
		local d = res.data:get()
		return if type(d) == "table" and type(d.servers) == "table" then d.servers else {}
	end)
	local function kind(): string
		if res.data:get() ~= nil then
			return if #servers:get() > 0 then "list" else "empty"
		end
		return if res.status:get() == "failed" then "failed" else "loading"
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
					return Row { now = now }
				end, {
					key = function(s: any): string
						return s.name
					end,
					wash = true,
				})
			end, { w_full = true, enter = { opacity = { 0, 1 }, duration = 120 }, exit = { opacity = 0, duration = 60 }, mode = "wait" }),
		}),
	}
end

return M
