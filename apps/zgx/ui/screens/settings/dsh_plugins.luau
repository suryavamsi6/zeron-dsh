-- Settings → DeepSeek plugins: the dsh bundles installed for DeepSeek
-- sessions on this device (the engine's `DshPlugins`, crates/engine/src/
-- dsh_plugins.rs), and installing one from a spec (`DshPluginAdd`, run by
-- `dsh plugin` on the device). What a bundle adds lands on the sessions
-- begun after, so the page's foot says so. The list is asked once as the
-- page opens (a `ui.resource`; Try again asks again, an install asks again
-- when it lands); installing needs pnpm 10 on the device, said before the
-- rows when it isn't there.

local host = require("@app/screens/settings/host")
local ds = require("@ds")
local search = require("@app/screens/settings/search")

local M = { ID = "dsh-plugins", TITLE = "DeepSeek plugins" }
search.index(M.ID, ui.t("settings.dsh_plugins.title"), ui.t("settings.dsh_plugins.terms"), {
	{ key = "add", title = ui.t("settings.dsh_plugins.add"), terms = ui.t("settings.dsh_plugins.add_terms") },
	{ key = "plugins", title = ui.t("settings.dsh_plugins.installed"), terms = ui.t("settings.dsh_plugins.installed_terms") },
})

-- What went wrong asking the host, in words.
local function say(err: any): string
	return if type(err) == "table" and type(err.message) == "string" then err.message else tostring(err)
end

-- What a failed install says: the engine's refusal when it has one (an
-- engines.dsh mismatch, already a sentence), else the run's log as its
-- last six lines (empty tail lines dropped).
local function why(answer: any): string
	if type(answer) == "table" then
		if type(answer.refusal) == "string" and answer.refusal ~= "" then
			return answer.refusal
		end
		local lines = string.split(if type(answer.log) == "string" then answer.log else "", "\n")
		while #lines > 0 and lines[#lines] == "" do
			table.remove(lines)
		end
		local tail = table.concat(lines, "\n", math.max(1, #lines - 5))
		if tail ~= "" then
			return tail
		end
	end
	return ui.t("settings.dsh_plugins.install_failed"):get()
end

-- Installing one from a spec: the field and Install beside it (Install off
-- while the field is empty or a run is on, the typed spec kept through it),
-- "Installing…" beside them, the answer under them. `on_added` asks the
-- list again.
local function install_row(on_added: () -> ()): Node
	local spec = ui.use_state("")
	local installing = ui.use_state(false)
	local note = ui.use_state(nil :: string?)
	local problem = ui.use_state(nil :: string?)
	local function install()
		local s = spec:peek()
		if s == "" or installing:peek() then
			return
		end
		installing:set(true)
		note:set(nil)
		problem:set(nil)
		host.call("DshPluginAdd", { spec = s }):after(function(answer: any)
			installing:set(false)
			if type(answer) == "table" and answer.ok == true then
				spec:set("")
				note:set(ui.t("settings.dsh_plugins.installed_ok"):get())
				on_added()
			else
				problem:set(why(answer))
			end
		end, function(err: any)
			installing:set(false)
			problem:set(say(err))
		end)
	end
	return ds.Settings.Row {
		label = ui.t("settings.dsh_plugins.package"),
		description = ui.t("settings.dsh_plugins.how"),
		stacked = true,
		control = ui.column {
			w_full = true,
			gap = "$space.2",
			ui.row {
				w_full = true,
				gap = "$space.2",
				items = "center",
				ds.TextField {
					value = spec,
					name = ui.t("settings.dsh_plugins.package"),
					placeholder = "dsh-hello-plugin",
					size = "sm",
					grow = 1,
					disabled = installing,
					on_commit = install,
				},
				ds.Button {
					ui.t("settings.dsh_plugins.install"),
					size = "sm",
					disabled = function(): boolean
						return installing:get() or spec:get() == ""
					end,
					on_click = install,
				},
				ui.show(installing, ds.Hint { ui.t("settings.dsh_plugins.installing") }),
			},
			ui.show(function(): boolean
				return note:get() ~= nil
			end, ds.Hint { function(): string
				return note:get() or ""
			end }),
			ui.show(function(): boolean
				return problem:get() ~= nil
			end, ui.div {
				w_full = true,
				rounded = "$radius.md",
				bg = "$surface.sunken",
				px = "$space.3",
				py = "$space.2",
				ui.text {
					function(): string
						return problem:get() or ""
					end,
					literal = true,
					wrap = true,
					font = "$font.mono",
					text_size = "$font.size.xs",
					line_height = "$font.line.xs",
					color = "$text.secondary",
				},
			}),
			ds.Hint { ui.t("settings.dsh_plugins.trust") },
		},
	}
end

-- A bundle's row: its name, its description under it, and at its end its
-- version and whether it's on, in words. A bundle whose client parts need
-- a browser (`needsClient`) says so on a second quiet line.
local function row(_b: any, _i: number, now: State<any>): Node
	return ds.Settings.Row {
		label = function(): string
			return now:get().name
		end,
		description = function(): string
			return now:get().description or ""
		end,
		detail = function(): string?
			return if now:get().needsClient == true then ui.t("settings.dsh_plugins.needs_client"):get() else nil
		end,
		truncate = true,
		control = ui.row {
			gap = "$space.3",
			items = "center",
			ui.show(function(): boolean
				local b = now:get()
				return b.version ~= nil or b.spec ~= nil
			end, ds.Badge {
				function(): string
					local b = now:get()
					return b.version or b.spec
				end,
				tone = "neutral",
				tabular = true,
			}),
			ui.text {
				function(): string
					return ui.t(if now:get().active then "settings.dsh_plugins.active" else "settings.dsh_plugins.not_active"):get()
				end,
				text_size = "$font.size.sm",
				line_height = "$font.line.sm",
				color = "$text.secondary",
			},
		},
	}
end

function M.view(): Node
	-- Asked as the page opens; Try again asks again.
	local res = ui.resource(function(): any
		return host.call("DshPlugins", {}):await()
	end)
	local bundles = ui.computed(function(): { any }
		local d = res.data:get()
		return if type(d) == "table" and type(d.bundles) == "table" then d.bundles else {}
	end)
	local function kind(): string
		if res.data:get() ~= nil then
			return if #bundles:get() > 0 then "list" else "empty"
		end
		return if res.status:get() == "failed" then "failed" else "loading"
	end
	return ds.Settings.Page {
		title = ui.t("settings.dsh_plugins.title"),
		description = ui.t("settings.dsh_plugins.description"),
		search.anchor("add", ds.Settings.Section {
			ds.Settings.Group {
				install_row(res.reload),
			},
		}),
		search.anchor("plugins", ds.Settings.Section {
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
						icon = "plug-circle",
						size = "sm",
						title = ui.t("settings.dsh_plugins.failed_title"),
						body = function(): string
							local err = res.error:get()
							return if err then `{say(err)} {ui.t("settings.dsh_plugins.failed_hint"):get()}` else ui.t("settings.dsh_plugins.failed_hint"):get()
						end,
						action = { label = ui.t("settings.dsh_plugins.retry"), on_click = res.reload },
					}
				elseif k == "empty" then
					return ds.EmptyState {
						icon = "plug-circle",
						size = "sm",
						title = ui.t("settings.dsh_plugins.empty_title"),
						body = ui.t("settings.dsh_plugins.empty_body"),
					}
				end
				return ui.column {
					w_full = true,
					gap = "$space.2",
					ui.show(function(): boolean
						local d = res.data:get()
						return type(d) == "table" and d.pnpm == false
					end, ui.row {
						w_full = true,
						px = ds.Settings.inset(),
						ds.Hint { ui.t("settings.dsh_plugins.needs_pnpm") },
					}),
					ds.Settings.Each(bundles, row, {
						key = function(b: any): string
							return b.name
						end,
						wash = true,
					}),
				}
			end, { w_full = true, enter = { opacity = { 0, 1 }, duration = 120 }, exit = { opacity = 0, duration = 60 }, mode = "wait" }),
		}),
		ui.show(function(): boolean
			return res.data:get() ~= nil
		end, ui.row {
			w_full = true,
			px = ds.Settings.inset(),
			ds.Hint { ui.t("settings.dsh_plugins.applies") },
		}),
	}
end

return M
