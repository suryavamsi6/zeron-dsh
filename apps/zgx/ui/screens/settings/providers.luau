-- Settings → Providers: the routes the run device's DSH gateway reports
-- (`llm/listConfigurableProviders`, through the engine's Dsh* calls,
-- crates/engine/src/dsh_providers.rs), each with the one control it needs:
-- a key field for an API-key route, the ChatGPT browser sign-in for the
-- chatgpt one, a quiet line for a route the account already covers. A key
-- goes to the gateway's credential store (`credentials/set`, read back
-- with `credentials/describe`); the sign-in is Zeron's own plugin's
-- (`zeron-chatgpt/*`), the browser finishing what Start opens.
--
-- Routes are the run device's: choosing another in the picker turns their
-- controls off (the engine answers these calls only for its own device),
-- rather than writing a key somewhere it wasn't asked.

local host = require("@app/screens/settings/host")
local ds = require("@ds")
local agents = require("@account/agents")
local devices = require("@app/screens/settings/devices")
local sessions = require("@account/sessions")
local visits = require("@app/screens/settings/visits")
local cloud = require("@account/cloud")
local search = require("@app/screens/settings/search")
local ModelVisibility = require("@app/model_visibility")

local M = { ID = "providers", TITLE = "Providers" }
search.index(M.ID, ui.t("settings.providers.title"), ui.t("settings.providers.routes.terms"), {
	{ key = "routes", title = ui.t("settings.providers.routes.title"), terms = ui.t("settings.providers.routes.terms") },
})

-- Sign-in polls status every 2 s for at most five minutes.
local POLL_MS = 2000
local POLL_MAX_MS = 300000

-- A row's trailing control: the lane Settings.Row keeps at its end
-- (`zeron.settings.control`), filled so the field, the buttons and the
-- state words end on one edge down the page.
local function lane(): number
	return (ui.theme.get("zeron.settings.control") :: number?) or 240
end

local function here(): string?
	for _, device in sessions.devices:get() or {} do
		if device.here then return device.id end
	end
	return nil
end

-- A device's name for the hint (the picker's choice is an id).
local function device_name(id: string): string?
	for _, device in sessions.devices:get() or {} do
		if device.id == id then return device.name end
	end
	return nil
end

-- One catalog entry, read as the engine reads it
-- (`parse_provider_catalog`): the pinned DSH's `nativeAuth`, the older
-- `apiKeyEnv`/`auth` fallbacks. Entries without a route or a settings
-- namespace are skipped, not guessed at.
-- The catalog's display names are its route ids for the pi-ai routes, which
-- read as duplicates (`openai`, `openai-codex`) or as one product twice
-- (`opencode`, `opencode-go`). Name them as people know them.
local NAMES: { [string]: string } = {
	openai = "OpenAI",
	opencode = "OpenCode Zen",
	["opencode-go"] = "OpenCode Go",
}
-- The Codex CLI's own OAuth route: the ChatGPT sign-in here is the `openai`
-- row, so a second OpenAI row is only a duplicate to read past.
local HIDDEN: { [string]: boolean } = { ["openai-codex"] = true }
-- Two products, one env name: pi-ai's catalog gives OpenCode Zen and OpenCode
-- Go the same `OPENCODE_API_KEY`, but a Zen key is not a Go key. Zen gets a
-- ref of its own; Go keeps the catalog's, where a saved key already lives.
local REFS: { [string]: string } = { opencode = "OPENCODE_ZEN_API_KEY" }

local function route_of(entry: any): any?
	if type(entry) ~= "table" or type(entry.provider) ~= "string" or type(entry.settingsNs) ~= "string" then
		return nil
	end
	if HIDDEN[entry.provider] then
		return nil
	end
	local native = if type(entry.nativeAuth) == "table" then entry.nativeAuth else {}
	local env = if type(native.env) == "table" then native.env[1] else nil
	local id = entry.provider
	-- Zeron's plugin signs in to OpenAI alone (`zeron-chatgpt`, whose
	-- `PROVIDER` is `openai`): another route's OAuth is the provider's own
	-- setup, not a button here.
	local chatgpt = id == "openai"
	local oauth = native.oauth == true and not chatgpt
	local account = not chatgpt
		and (entry.account == true or entry.auth == "account" or string.find(id, "deepseek") ~= nil)
	return {
		id = id,
		name = NAMES[id] or (if type(entry.displayName) == "string" then entry.displayName else id),
		settings_ns = entry.settingsNs,
		key_ref = REFS[id] or (if type(env) == "string" then env elseif type(entry.apiKeyEnv) == "string" then entry.apiKeyEnv else nil),
		chatgpt = chatgpt,
		oauth = oauth,
		account = account,
	}
end

-- Whether `ref` reads back as saved from a `credentials/describe` answer
-- ({REF: {configured: true, ...}}).
local function configured_in(creds: any, ref: string): boolean
	local c = if type(creds) == "table" then creds[ref] else nil
	return type(c) == "table" and c.configured == true
end

local function say(err: any): string
	return if type(err) == "table" and type(err.message) == "string" then err.message else tostring(err)
end

-- An API-key route's trailing control: the masked field and Save beside
-- it. Saving clears the field only while nobody typed over it, and marks
-- the ref configured in place (no reload).
local function key_row(item: any, remote: () -> boolean, creds: State<any>): Node
	local key = ui.use_state("")
	local saving = ui.use_state(false)
	local save_err = ui.use_state(nil :: string?)
	local note = ui.use_state(nil :: string?)
	local ref = item.key_ref
	local function save()
		if remote() or saving:peek() then
			return
		end
		local value = key:peek()
		if string.find(value, "%s") then
			save_err:set(ui.t("settings.providers.routes.key.whitespace"):get())
			note:set(nil)
			return
		end
		saving:set(true)
		save_err:set(nil)
		note:set(nil)
		host.call("DshSetCredential", { ref = ref, value = value }):after(function()
			-- A stored key alone does nothing: pi-ai resolves the route's
			-- `profile.apiKeyEnv` through the credential store, and falls back
			-- to the environment variable when the profile names none
			-- (`dsh-llm-pi-ai`: MISSING_CREDENTIAL). One leaf-addressed set
			-- names it without touching the rest of the route's profile.
			if item.settings_ns == "llm-pi-ai" then
				host.call("DshMutateSettings", {
					ns = item.settings_ns,
					ops = { { op = "set", path = { "providers", item.id, "apiKeyEnv" }, value = ref } },
				})
			end
			saving:set(false)
			if key:peek() == value then
				key:set("")
			end
			creds:set_key(ref, { configured = true })
			note:set(ui.t("settings.providers.routes.key.saved"):get())
		end, function(err: any)
			saving:set(false)
			save_err:set(ui.t("settings.providers.routes.key.failed", { says = say(err) }):get())
		end)
	end
	return ds.Settings.Row {
		label = item.name,
		description = ui.t("settings.providers.routes.key.description", { ref = ref }),
		badge = ui.show(function(): boolean
			return configured_in(creds:get(), ref)
		end, ds.Badge { ui.t("settings.providers.routes.key.configured"), tone = "success", size = "sm" }),
		detail = function(): string?
			return note:get()
		end,
		control = ui.row {
			w = lane(),
			shrink = 0,
			gap = "$space.2",
			items = "start",
			ds.TextField {
				value = key,
				grow = 1,
				size = "sm",
				password = true,
				name = ui.t("settings.providers.routes.key.name", { name = item.name }),
				placeholder = ui.t("settings.providers.routes.key.placeholder"),
				disabled = remote,
				error = save_err,
				on_commit = save,
			},
			ds.Button {
				ui.t("settings.providers.routes.key.save"),
				size = "sm",
				busy = saving,
				disabled = function(): boolean
					return remote() or saving:get() or key:get() == ""
				end,
				on_click = save,
			},
		},
	}
end

-- The chatgpt route's trailing control: its one button, signing in with
-- the browser (Start opens the page, the plugin's status answers while it
-- waits, Cancel or Escape ends it; a poll runs at most five minutes) or
-- reading Signed in once the grant is there.
local function chatgpt_row(item: any, remote: () -> boolean): Node
	local phase = ui.use_state(if item.authorized then "signed_in" else "idle")
	local tried = ui.use_state(false)
	local note = ui.use_state(nil :: string?)
	-- The wait's poll: the click that started it is long gone by the time
	-- status answers, so the work is one task that ends with the wait (a
	-- cancel, an answer, the time's up, or the row going — tasks a row
	-- makes are cancelled with it). A second call while one runs is a
	-- no-op: it leaves at the first phase check.
	local function wait_for()
		local deadline = ui.now() + POLL_MAX_MS
		while ui.now() < deadline do
			if phase:peek() ~= "waiting" then
				return
			end
			ui.sleep(POLL_MS):await()
			if phase:peek() ~= "waiting" then
				return
			end
			local ok, status = pcall(function(): any
				return host.call("DshChatgptStatus", {}):await()
			end)
			if phase:peek() ~= "waiting" then
				return
			end
			if ok and type(status) == "table" then
				if status.authorized == true then
					phase:set("signed_in")
					return
				elseif status.running ~= true then
					phase:set("idle")
					tried:set(true)
					note:set(ui.t("settings.providers.routes.sign_in.not_completed"):get())
					return
				end
			end
		end
		if phase:peek() == "waiting" then
			-- A wait over: let the gateway's attempt go too, or a late
			-- finish in the browser would still sign in.
			phase:set("idle")
			tried:set(true)
			note:set(ui.t("settings.providers.routes.sign_in.not_completed"):get())
			host.call("DshChatgptCancel", {}):after(function() end, function() end)
		end
	end
	local poll = ui.async(wait_for)
	local function start()
		local p = phase:peek()
		if remote() or (p ~= "idle" and p ~= "failed") then
			return
		end
		phase:set("starting")
		note:set(nil)
		host.call("DshChatgptStart", {}):after(function(answer: any)
			if phase:peek() ~= "starting" then
				return
			end
			phase:set("waiting")
			local url = if type(answer) == "table" and type(answer.url) == "string" then answer.url else nil
			if url then
				pcall(host.invoke, "open_url", url)
			end
			poll()
		end, function(err: any)
			if phase:peek() == "starting" then
				phase:set("idle")
				tried:set(true)
				note:set(ui.t("settings.providers.routes.sign_in.failed", { says = say(err) }):get())
			end
		end)
	end
	local function cancel()
		if phase:peek() ~= "waiting" then
			return
		end
		phase:set("cancelling")
		host.call("DshChatgptCancel", {}):after(function()
			phase:set("idle")
			-- Whether the browser finished first: ask once, so a grant it
			-- landed still reads Signed in.
			host.call("DshChatgptStatus", {}):after(function(status: any)
				if type(status) == "table" and status.authorized == true then
					phase:set("signed_in")
				end
			end, function() end)
		end, function()
			phase:set("waiting")
		end)
	end
	return ds.Settings.Row {
		label = item.name,
		description = ui.t("settings.providers.routes.sign_in.description"),
		badge = ui.show(function(): boolean
			return phase:get() == "signed_in"
		end, ds.Badge { ui.t("settings.providers.routes.sign_in.signed_in"), tone = "success", size = "sm" }),
		detail = function(): string?
			return note:get()
		end,
		control = ui.row {
			w = lane(),
			shrink = 0,
			gap = "$space.2",
			items = "center",
			justify = "end",
			ui.show(function(): boolean
				return phase:get() == "waiting"
			end, ui.text {
				ui.t("settings.providers.routes.sign_in.waiting"),
				text_size = "$font.size.sm",
				color = "$text.secondary",
			}),
			ui.show(function(): boolean
				return phase:get() ~= "signed_in"
			end, ds.Button {
				function(): string
					return if phase:get() == "waiting" or phase:get() == "cancelling"
						then ui.t("settings.providers.routes.sign_in.cancel"):get()
						elseif tried:get()
						then ui.t("settings.providers.routes.sign_in.try_again"):get()
						else ui.t("settings.providers.routes.sign_in.start"):get()
				end,
				tone = "secondary",
				size = "sm",
				busy = function(): boolean
					return phase:get() == "starting" or phase:get() == "cancelling"
				end,
				disabled = function(): boolean
					return remote() or phase:get() == "starting" or phase:get() == "cancelling"
				end,
				on_key = function(e: any): boolean?
					if e.key == "Escape" and phase:peek() == "waiting" then
						cancel()
						return true
					end
					return nil
				end,
				on_click = function()
					if phase:peek() == "waiting" then
						cancel()
					else
						start()
					end
				end,
			}),
		},
	}
end

-- A route's row by the control it needs (the engine's own precedence:
-- its sign-in, then its key, then the account it's already part of).
local function route_row(item: any, remote: () -> boolean, creds: State<any>): Node
	if item.chatgpt then
		return chatgpt_row(item, remote)
	elseif item.key_ref then
		return key_row(item, remote, creds)
	elseif item.account then
		return ds.Settings.Row {
			label = item.name,
			description = ui.t("settings.providers.routes.account"),
		}
	elseif item.oauth then
		return ds.Settings.Row {
			label = item.name,
			description = ui.t("settings.providers.routes.oauth_elsewhere"),
		}
	end
	return ds.Settings.Row {
		label = item.name,
		description = ui.t("settings.providers.routes.nothing"),
	}
end

-- The gateway's answer, as the page needs it: the parsed routes, which of
-- their credential refs are saved, and whether the chatgpt grant is there
-- already (its status asked once at load; the wait polls it later).
local function load(): any
	local answer = host.call("DshProviders", {}):await()
	local entries = if type(answer) == "table" and type(answer.providers) == "table" then answer.providers else answer
	local routes: { any } = {}
	local refs: { string } = {}
	local want_status = false
	if type(entries) == "table" then
		for _, entry in entries do
			local route = route_of(entry)
			if route then
				table.insert(routes, route)
				if route.key_ref then
					table.insert(refs, route.key_ref)
				end
				if route.chatgpt then
					want_status = true
				end
			end
		end
	end
	local creds = nil
	if #refs > 0 then
		creds = host.call("DshCredentials", { refs = refs }):await()
	end
	-- What each route serves, for the Models section's counts.
	local counts: { [string]: number } = {}
	local listed = host.call("ListModels", { harness = "deepseek" }):await()
	local models = if type(listed) == "table" and type(listed.models) == "table" then listed.models else listed
	if type(models) == "table" then
		for _, model in models do
			local provider = if type(model) == "table" then model.provider else nil
			local id = if type(provider) == "table" then provider.id else nil
			if type(id) == "string" then
				counts[id] = (counts[id] or 0) + 1
			end
		end
	end
	local authorized = false
	if want_status then
		local ok, status = pcall(function(): any
			return host.call("DshChatgptStatus", {}):await()
		end)
		authorized = ok and type(status) == "table" and status.authorized == true
	end
	for i, route in routes do
		route.order = i
		if route.chatgpt then
			route.authorized = authorized
		end
	end
	return { routes = routes, creds = creds, counts = counts }
end

-- A route's models, on or off: what the composer's picker offers.
local function model_row(item: any): Node
	local on = ui.use_state(ModelVisibility.showing(item.id))
	return ds.Settings.Row {
		label = ui.t("settings.providers.models.every", { name = item.name }),
		description = if item.count == 1
			then ui.t("settings.providers.models.count_one")
			else ui.t("settings.providers.models.count", { count = item.count }),
		control = ds.Switch {
			value = on,
			label = item.name,
			on_change = function(value: boolean)
				ModelVisibility.set_showing(item.id, value)
			end,
		},
	}
end

function M.body(): Node
	return ui.component(function(): Node
		-- The catalog and what it needs, loaded once a visit (Try again
		-- loads again); the refs' saved state beside it, rows writing
		-- theirs when a key lands.
		local res = ui.resource(load)
		local creds = ui.use_state({} :: any)
		ui.effect(function()
			local d = res.data:get()
			if d then
				creds:set(if type(d.creds) == "table" then d.creds else {})
			end
		end)
		-- What is connected first: a saved key or a grant in place is what a
		-- visit is usually about, and the rest is a long tail. Read here, not
		-- in `load`, so a key saved on this visit moves its row at once.
		-- (By the catalog's own order within each group: `table.sort` is not
		-- stable.)
		local catalog = ui.computed(function(): { any }
			local d = res.data:get()
			local routes = if d then table.clone(d.routes) else {}
			local saved = creds:get()
			local function ready(route: any): boolean
				if route.chatgpt then
					return route.authorized == true
				end
				return route.key_ref ~= nil and configured_in(saved, route.key_ref)
			end
			table.sort(routes, function(a: any, b: any): boolean
				local a_ready, b_ready = ready(a), ready(b)
				if a_ready ~= b_ready then
					return a_ready
				end
				return a.order < b.order
			end)
			return routes
		end)
		local function remote(): boolean
			local target = agents.now:get().target
			return target ~= nil and target ~= here()
		end
		-- A block per provider, as the upstream screen reads: its name over
		-- its own row (the account or key it takes), not one long list.
		local rows = ui.column {
			w_full = true,
			gap = "$space.4",
			ds.Settings.Each(catalog, function(item: any, _i: number, _now: State<any>): Node
				return ds.Settings.Section {
					title = item.name,
					ds.Settings.Group { route_row(item, remote, creds) },
				}
			end, { key = function(item: any): string
				return item.id
			end }),
			ui.show(function(): boolean
				return res.error:get() ~= nil
			end, ui.row {
				w_full = true,
				px = ds.Settings.inset(),
				gap = "$space.2",
				items = "center",
				ds.Hint { function(): string
					return res.error:get() or ""
				end },
				ds.Button {
					ui.t("settings.providers.routes.failed.retry"),
					tone = "secondary",
					size = "sm",
					on_click = res.reload,
				},
			}),
			ui.show(function(): boolean
				return res.loading:get()
			end, ui.row {
				w_full = true,
				px = ds.Settings.inset(),
				ds.Hint { ui.t("settings.providers.routes.refreshing") },
			}),
		}
		-- The routes that serve models, in the catalog's order.
		local model_routes = ui.computed(function(): { any }
			local d = res.data:get()
			if not d then
				return {}
			end
			local counts = if type(d.counts) == "table" then d.counts else {}
			local out = {}
			for _, route in d.routes do
				local count = counts[route.id]
				if type(count) == "number" and count > 0 then
					table.insert(out, { id = route.id, name = route.name, count = count })
				end
			end
			return out
		end)
		return ui.column {
			w_full = true,
			gap = "$space.6",
			search.anchor(
				"routes",
				ds.Settings.Section {
					ui.show(remote, ds.Hint { function(): string
						local target = agents.now:get().target
						if target == nil then return "" end
						local name = if target == "cloud" then "Cloud" else (device_name(target) or target)
						return ui.t("settings.providers.routes.remote", { device = name }):get()
					end }),
					ui.show(function(): boolean
						return res.data:get() == nil and res.status:get() == "loading"
					end, ds.Settings.Group {
						ds.SkeletonRow { title = 90, line = 200, trail = 120 },
						ds.SkeletonRow { title = 110, line = 170, trail = 120 },
					}),
					ui.show(function(): boolean
						return res.data:get() == nil and res.status:get() == "failed"
					end, ds.EmptyState {
						icon = "cloud",
						title = ui.t("settings.providers.routes.failed.title"),
						body = function(): string
							local err = res.error:get()
							return if err then `{err} {ui.t("settings.providers.routes.failed.hint"):get()}` else ui.t("settings.providers.routes.failed.hint"):get()
						end,
						action = { label = ui.t("settings.providers.routes.failed.retry"), on_click = res.reload },
					}),
					ui.show(function(): boolean
						return res.data:get() ~= nil and #catalog:get() == 0
					end, ds.EmptyState {
						icon = "widget",
						title = ui.t("settings.providers.routes.empty.title"),
						body = ui.t("settings.providers.routes.empty.body"),
					}),
					ui.show(function(): boolean
						return #catalog:get() > 0
					end, rows),
				}
			),
			-- Which of the routes' models the composer offers.
			search.anchor(
				"models",
				ds.Settings.Section {
					title = ui.t("settings.providers.models.title"),
					description = ui.t("settings.providers.models.description"),
					ui.show(function(): boolean
						return #model_routes:get() == 0
					end, ui.row {
						w_full = true,
						px = ds.Settings.inset(),
						ds.Hint { ui.t("settings.providers.models.none") },
					}),
					ds.Settings.Group {
						ds.Settings.Each(model_routes, function(item: any): Node
							return model_row(item)
						end, { key = function(item: any): string
							return item.id
						end }),
					},
				}
			),
		}
	end)({})
end

function M.view(): Node
	visits.once("providers", function() agents.load() end)
	return ds.Settings.Page {
		title = "Providers",
		description = ui.t("settings.providers.routes.page_description"),
		trail = devices.picker(function() return agents.now:get().target or here() end, function(id)
			agents.target(if id == here() then nil else id)
		end, function()
			return if cloud.device() then { { value = "cloud", label = "Cloud", icon = "cloud" } } else {}
		end),
		trail_top = true,
		M.body(),
	}
end

return M
