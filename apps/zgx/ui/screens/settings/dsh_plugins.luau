-- Settings → DeepSeek plugins: the dsh bundles installed for DeepSeek
-- sessions on this device (the engine's `DshPlugins`, crates/engine/src/
-- dsh_plugins.rs), installing one from a spec (`DshPluginAdd`), and
-- removing one (`DshPluginRemove`, asked once by name first), both run by
-- `dsh plugin` on the device. Under each bundle the rows its patch adds
-- (`DshPluginRows`), a Switch each — the override is the profile's
-- cordis.patch.yml (`DshPluginSetRow` writes it, the same place dsh's own
-- plugin manager does), a row it can't safely write shown off with its
-- reason. What a bundle or row stops landing lands on the sessions begun
-- after, so the page's foot says so. The list is asked once as the page
-- opens (a `ui.resource`; Try again asks again, an install or remove asks
-- again when it lands); installing needs pnpm 10 on the device, said
-- before the rows when it isn't there.

local host = require("@app/screens/settings/host")
local ds = require("@ds")
local search = require("@app/screens/settings/search")

local M = { ID = "dsh-plugins", TITLE = "DeepSeek plugins" }
search.index(M.ID, ui.t("settings.dsh_plugins.title"), ui.t("settings.dsh_plugins.terms"), {
	{ key = "add", title = ui.t("settings.dsh_plugins.add"), terms = ui.t("settings.dsh_plugins.add_terms") },
	{ key = "discover", title = ui.t("settings.dsh_plugins.discover"), terms = ui.t("settings.dsh_plugins.discover_terms") },
	{ key = "plugins", title = ui.t("settings.dsh_plugins.installed"), terms = ui.t("settings.dsh_plugins.installed_terms") },
})

-- The locale the app shows (`screens/language`): an index's own words
-- follow it, so a fetch asks for its entries in it.
local shown = ui.shared("zeron.locale", "en")

-- What went wrong asking the host, in words.
local function say(err: any): string
	return if type(err) == "table" and type(err.message) == "string" then err.message else tostring(err)
end

-- What a failed run says: the engine's refusal when it has one (an
-- engines.dsh mismatch, already a sentence), else the run's log as its
-- last six lines (empty tail lines dropped), else `generic`.
local function why(answer: any, generic: string): string
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
	return ui.t(generic):get()
end

-- One `DshPluginAdd` run for `spec` (a name, a name@version, a URL): its
-- answer dispatched to `h` — `h.asked(package)` when a build script wants
-- letting through (`needsBuildApproval`, the package pnpm would build),
-- `h.done()` on ok, `h.failed(words)` on anything else. `allow` names the
-- package the asking let through, for the retry.
local function install_call(spec: string, allow: string?, h: { asked: (string) -> (), done: () -> (), failed: (string) -> () })
	host.call("DshPluginAdd", { spec = spec, allowBuild = allow }):after(function(answer: any)
		if type(answer) == "table" and type(answer.needsBuildApproval) == "string" then
			h.asked(answer.needsBuildApproval)
		elseif type(answer) == "table" and answer.ok == true then
			h.done()
		else
			h.failed(why(answer, "settings.dsh_plugins.install_failed"))
		end
	end, function(err: any)
		h.failed(say(err))
	end)
end

-- Installing one from a spec: the field and Install beside it (Install off
-- while the field is empty or a run is on, the typed spec kept through it),
-- "Installing…" beside them, the answer under them. `on_added` asks the
-- list again. A run refused for a build script answers the package pnpm
-- would build (`needsBuildApproval`), which asks once — Cancel only closes
-- it, "Allow and install" runs the spec again with `allowBuild` naming it.
local function install_row(on_added: () -> ()): Node
	local spec = ui.use_state("")
	local installing = ui.use_state(false)
	local note = ui.use_state(nil :: string?)
	local problem = ui.use_state(nil :: string?)
	-- The package whose build script wants letting through, while its
	-- asking is up.
	local asking = ui.use_state(nil :: string?)
	local ask_open = ui.use_state(false)
	local function install(allow: string?)
		local s = spec:peek()
		if s == "" or installing:peek() then
			return
		end
		installing:set(true)
		note:set(nil)
		problem:set(nil)
		install_call(s, allow, {
			asked = function(name: string)
				installing:set(false)
				asking:set(name)
				ask_open:set(true)
			end,
			done = function()
				installing:set(false)
				spec:set("")
				note:set(ui.t("settings.dsh_plugins.installed_ok"):get())
				on_added()
			end,
			failed = function(words: string)
				installing:set(false)
				problem:set(words)
			end,
		})
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
					on_commit = function()
						install()
					end,
				},
				ds.Button {
					ui.t("settings.dsh_plugins.install"),
					size = "sm",
					disabled = function(): boolean
						return installing:get() or spec:get() == ""
					end,
					on_click = function()
						install()
					end,
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
			-- (Its asking, beside the row's control: the modal stands over
			-- the page wherever it is built.)
			ds.Dialog {
				open = ask_open,
				title = (function(): string
					return ui.t("settings.dsh_plugins.allow_build_title", { name = asking:get() or "" }):get()
				end) :: any,
				body = ui.t("settings.dsh_plugins.allow_build_body"),
				tone = "danger",
				size = "sm",
				confirm = {
					label = ui.t("settings.dsh_plugins.allow_build_confirm"),
					on_click = function()
						install(asking:peek())
					end,
				},
			},
		},
	}
end

-- One row of a bundle's patch: its id, its title under it (or why it can't
-- be switched), a Switch for its `disabled` (on is on). Toggling writes
-- `{id, disabled}` to the profile's cordis.patch.yml through the host; the
-- answer is the bundle's rows again, which the parent's list takes whole.
-- A refused write puts the switch back and says why on the row.
local PatchRow = ui.component(function(props: { now: State<any>, answered: (rows: any) -> () }): Node
	local on = ui.use_state(false)
	local busy = ui.use_state(false)
	local problem = ui.use_state(nil :: string?)
	-- The switch reads the row's `disabled`; a refreshed row re-reads it.
	ui.effect(function()
		local r = props.now:get()
		on:set(not (type(r) == "table" and r.disabled == true))
		problem:set(nil)
	end)
	local function row(): any
		return props.now:get()
	end
	local function flip(on_next: boolean)
		local r = props.now:peek()
		if type(r) ~= "table" or type(r.id) ~= "string" then
			return
		end
		-- Shown at once; the answer's refreshed rows (or a refusal) settle it.
		on:set(on_next)
		busy:set(true)
		problem:set(nil)
		host.call("DshPluginSetRow", { id = r.id, disabled = not on_next }):after(function(answer: any)
			busy:set(false)
			if type(answer) == "table" and type(answer.rows) == "table" then
				props.answered(answer.rows)
			else
				on:set(not on_next)
				problem:set(why(answer, "settings.dsh_plugins.row_failed"))
			end
		end, function(err: any)
			busy:set(false)
			on:set(not on_next)
			problem:set(say(err))
		end)
	end
	return ds.Settings.Row {
		compact = true,
		label = function(): string
			local r = row()
			return if type(r) == "table" and type(r.id) == "string" then r.id else ""
		end,
		description = function(): any
			local r = row()
			if type(r) ~= "table" then
				return ""
			end
			if r.editable == false then
				return if type(r.note) == "string" then r.note else ui.t("settings.dsh_plugins.row_locked"):get()
			end
			return r.title or ""
		end,
		detail = function(): string?
			return problem:get()
		end,
		control = ds.Switch {
			value = on,
			compact = true,
			label = function(): string
				local r = row()
				return if type(r) == "table" and type(r.id) == "string" then r.id else ""
			end,
			disabled = function(): boolean
				local r = props.now:peek()
				return (type(r) == "table" and r.editable == false) or busy:get()
			end,
			on_change = flip,
		},
	}
end)

-- A bundle's row: its name, its description under it, and at its end its
-- version and whether it's on, in words, then a quiet Remove that asks
-- once ("Remove <name>?") before `DshPluginRemove` runs — off while its
-- run is on. A bundle whose client parts need a browser (`needsClient`)
-- says so on a second quiet line, where a failed remove says why instead.
-- Under the row, the rows its patch carries, a switch each (`PatchRow`).
local Row = ui.component(function(props: { now: State<any>, reload: () -> () }): Node
	local now = props.now
	local open = ui.use_state(false)
	local removing = ui.use_state(false)
	local problem = ui.use_state(nil :: string?)
	-- The bundle's patch rows, asked once as the row builds; a row write's
	-- answer lists them again (`listed`).
	local rows_res = ui.resource(function(): any
		return host.call("DshPluginRows", { name = now:peek().name }):await()
	end)
	local listed = ui.use_state(nil :: any)
	ui.effect(function()
		local d = rows_res.data:get()
		if d ~= nil then
			listed:set(d)
		end
	end)
	local rows = ui.computed(function(): { any }
		local d = listed:get()
		return if type(d) == "table" and type(d.rows) == "table" then d.rows else {}
	end)
	local function remove()
		if removing:peek() then
			return
		end
		removing:set(true)
		problem:set(nil)
		host.call("DshPluginRemove", { name = now:peek().name }):after(function(answer: any)
			removing:set(false)
			if type(answer) == "table" and answer.ok == true then
				props.reload()
			else
				problem:set(why(answer, "settings.dsh_plugins.remove_failed"))
			end
		end, function(err: any)
			removing:set(false)
			problem:set(say(err))
		end)
	end
	return ui.column {
		w_full = true,
		ds.Settings.Row {
		label = function(): string
			return now:get().name
		end,
		description = function(): string
			return now:get().description or ""
		end,
		detail = function(): string?
			if problem:get() ~= nil then
				return problem:get()
			end
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
			ds.IconButton {
				icon = "trash-bin-minimalistic",
				label = function(): string
					return ui.t("settings.dsh_plugins.remove_named", { name = now:get().name }):get()
				end,
				size = "sm",
				tone = "danger",
				disabled = removing,
				on_click = function()
					open:set(true)
				end,
			},
			-- (Its asking, beside the row's control: the modal stands over
			-- the page wherever it is built.)
			ds.Dialog {
				open = open,
				title = (function(): string
					return ui.t("settings.dsh_plugins.remove_title", { name = now:get().name }):get()
				end) :: any,
				body = ui.t("settings.dsh_plugins.remove_body"),
				tone = "danger",
				size = "sm",
				confirm = {
					label = ui.t("settings.dsh_plugins.remove_confirm"),
					on_click = remove,
				},
			},
		},
		},
		-- The bundle's patch rows under it, a switch each, set in a touch to
		-- read as its own.
		ui.column {
			w_full = true,
			pl = "$space.4",
			ui.each(rows, function(_r: any, _i: number, row: State<any>): Node
				return PatchRow {
					now = row,
					answered = function(next: any)
						listed:set({ rows = next })
					end,
				}
			end, {
				key = function(r: any): any
					return r.id
				end,
			}),
		},
	}
end)

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
					ds.Settings.Each(bundles, function(_b: any, _i: number, now: State<any>): Node
						return Row { now = now, reload = res.reload }
					end, {
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
