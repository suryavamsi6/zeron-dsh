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
local chat = require("@app/screens/chat/store")
local Menu = ds.Menu

local M = { ID = "providers", TITLE = "Providers" }
search.index(M.ID, ui.t("settings.providers.title"), ui.t("settings.providers.routes.terms"), {
	{ key = "routes", title = ui.t("settings.providers.routes.title"), terms = ui.t("settings.providers.routes.terms") },
})

-- Sign-in polls status every 2 s for at most five minutes.
local POLL_MS = 2000
local POLL_MAX_MS = 300000

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
-- need clearer names (`openai-codex`, `opencode`, `opencode-go`). Name
-- them as people know them.
local NAMES: { [string]: string } = {
	openai = "OpenAI",
	["openai-codex"] = "Codex",
	opencode = "OpenCode Zen",
	["opencode-go"] = "OpenCode Go",
	mistral = "Mistral Vibe",
}

-- Provider SVGs have white source ink; AgentMark's `src` path tints them
-- with the theme's foreground on its quiet tile.
local MARKS: { [string]: string } = {
	openai = "provider-openai", anthropic = "provider-anthropic",
	openrouter = "provider-openrouter", google = "provider-gemini",
	gemini = "provider-gemini", xai = "provider-xai",
	deepseek = "provider-deepseek", ["deepseek-official"] = "provider-deepseek",
	mistral = "registry/mistral-vibe", ["kimi-coding"] = "registry/kimi",
	["qwen-code"] = "registry/qwen-code",
}
-- Two products, one env name: pi-ai's catalog gives OpenCode Zen and OpenCode
-- Go the same `OPENCODE_API_KEY`, but a Zen key is not a Go key. Zen gets a
-- ref of its own; Go keeps the catalog's, where a saved key already lives.
local REFS: { [string]: string } = { opencode = "OPENCODE_ZEN_API_KEY" }

local function route_of(entry: any): any?
	if type(entry) ~= "table" or type(entry.provider) ~= "string" or type(entry.settingsNs) ~= "string" then
		return nil
	end
	local native = if type(entry.nativeAuth) == "table" then entry.nativeAuth else {}
	local env = if type(native.env) == "table" then native.env[1] else nil
	local id = entry.provider
	-- This screen presents the ChatGPT subscription through Codex, whose
	-- account usage and banked resets are available here. Keep the legacy
	-- OpenAI route in the gateway/model catalog, but don't show a duplicate
	-- standalone account card.
	if id == "openai" then return nil end
	-- OpenAI and Codex use distinct grants and are signed in independently.
	local chatgpt = id == "openai" or id == "openai-codex"
	local oauth = (native.oauth == true and not chatgpt) or id == "mistral"
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

-- Fixed columns align the labels, bars and percentages in each card.
local function usage_meter(label: string, value: any): Node
	-- (A number, or a function of the state: a row is built once and its
	-- meters follow the account as it loads.)
	local read = if type(value) == "function" then value else function(): any
		return value
	end
	local function fraction(): number
		return math.max(0, math.min(1, tonumber(read()) or 0))
	end
	return ui.row {
		items = "center",
		gap = "$space.2",
		ui.text {
			label,
			w = ui.raw(44, "the compact usage label column"),
			color = "$text.secondary",
			text_size = "$font.size.xs",
		},
		ds.ProgressBar {
			value = fraction,
			w = ui.raw(76, "the compact usage bar column"),
			size = "thin",
		},
		ui.text {
			function(): string
				return `{math.floor(fraction() * 100 + 0.5)}%`
			end,
			w = ui.raw(28, "the compact usage percent column"),
			color = "$text.secondary",
			text_size = "$font.size.xs",
			align = "end",
		},
	}
end

-- The agent account behind a route, when one is signed in: its usage
-- windows are what the meters draw. Match only an explicit route ID: an
-- agent's account must not be attributed to every similarly named route.
local function account_for(route: any): any?
	local now = agents.now:get()
	for _, agent in now.list or {} do
		local id = agent.id
		if type(id) == "string" and route.id == id then
			for _, account in agent.accounts or {} do
				if account.active then
					return account
				end
			end
		end
	end
	return nil
end

-- A signed-in provider's plan, as the upstream screen draws it: its windows
-- (label, bar, percent) — nothing for a provider with no account or no plan.
local function route_usage(item: any): Node
	local function of(key: string): any
		return function(): any
			local account = account_for(item)
			return if account then account[key] else nil
		end
	end
	return ui.column {
		gap = "$space.1",
		usage_meter("Session", of("session")),
		usage_meter("Week", of("week")),
	}
end

local function has_usage(item: any): boolean
	local a = account_for(item)
	return a ~= nil and a.key ~= true and type(a.session) == "number" and type(a.week) == "number"
end

-- The compact credential or account mark in a provider card.
local function route_lead(icon: string): Node
	return ui.row {
		w = ui.raw(28, "the provider card credential disc"),
		h = ui.raw(28, "the provider card credential disc"),
		shrink = 0,
		items = "center",
		justify = "center",
		rounded = "$radius.full",
		bg = "$zeron.secondary.bg",
		color = "$text.secondary",
		ds.Icon { name = icon :: any, size = 16 },
	}
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
	local open = ui.use_state(false)
	return ui.column {
		w_full = true,
		ui.show(function(): boolean return has_usage(item) end, ds.Settings.Row {
			compact = true, private_label = true,
			label = function(): string local a = account_for(item) return if a then a.email or "" else "" end,
			description = function(): string local a = account_for(item) return if a then a.plan or "" else "" end,
			lead = ds.Avatar { seed = item.id, size = 28 }, control = route_usage(item),
		}),
		ds.Settings.Row {
			compact = true,
			label = function(): string
				return if configured_in(creds:get(), ref) then `API key · {ref}` else `Add API key · {item.name}`
			end,
			lead = route_lead(if configured_in(creds:peek(), ref) then "lock-keyhole-unlocked" else "plus"),
			badge = ui.show(function(): boolean return item.in_use == true end,
				ds.Badge { ui.t("settings.providers.routes.in_use"), tone = "accent", size = "sm" }),
			on_click = function() open:set(not open:peek()) end,
			disabled = remote,
		},
		ui.show(function(): boolean return open:get() end, ui.column {
			w_full = true,
			px = ui.raw(15, "the provider card inset"),
			pb = "$space.3",
			gap = "$space.2",
			ui.text { ui.t("settings.providers.routes.key.description", { ref = ref }), text_size = "$font.size.sm", color = "$text.secondary" },
			ui.row {
				w_full = true, gap = "$space.2", items = "center",
				ds.TextField {
					value = key, grow = 1, size = "sm", password = true,
					name = ui.t("settings.providers.routes.key.name", { name = item.name }),
					placeholder = ui.t("settings.providers.routes.key.placeholder"),
					disabled = remote, error = save_err, on_commit = save,
				},
				ds.Button {
					ui.t("settings.providers.routes.key.save"), size = "sm", busy = saving,
					disabled = function(): boolean return remote() or saving:get() or key:get() == "" end,
					on_click = save,
				},
			},
			ui.show(function(): boolean return note:get() ~= nil end, ds.Hint { function(): string return note:get() or "" end }),
		}),
	}
end

-- The chatgpt route's trailing control: its one button, signing in with
-- the browser (Start opens the page, the plugin's status answers while it
-- waits, Cancel or Escape ends it; a poll runs at most five minutes) or
-- reading Signed in once the grant is there.
local function chatgpt_row(item: any, remote: () -> boolean): Node
	local phase = ui.use_state(if item.authorized then "signed_in" else "idle")
	local identity = ui.use_state({ email = item.email, plan = item.plan })
	local accounts = ui.use_state({} :: { any })
	local tried = ui.use_state(false)
	local note = ui.use_state(nil :: string?)
	local account_note = ui.use_state(nil :: string?)
	local busy = ui.use_state(false)
	local accounts_loaded = ui.use_state(false)
	local attempt_id: string? = nil
	local reset_open = ui.use_state(false)
	local reset_choice = ui.use_state("")
	local reset_account = ui.use_state(nil :: any)
	local generation = 0
	local serial = 0
	local reset_key: string? = nil
	local reset_key_for: string? = nil
	local function refresh()
		generation += 1
		if remote() then return end
		local requested = generation
		host.call("DshChatgptAccounts", { provider = item.id }):after(function(answer: any)
			if requested ~= generation then return end
			if type(answer) == "table" and answer.provider == item.id and type(answer.accounts) == "table" then
				accounts:set(answer.accounts)
				accounts_loaded:set(true)
				if answer.usageError then account_note:set(ui.t("settings.providers.accounts.usage_unavailable"):get()) end
			else
				account_note:set(ui.t("settings.providers.accounts.load_failed"):get())
			end
		end, function()
			if requested == generation then account_note:set(ui.t("settings.providers.accounts.load_failed"):get()) end
		end)
	end
	ui.effect(function() refresh() end)
	local function signed_phase(): string
		return if item.authorized or #accounts:peek() > 0 or identity:peek().email then "signed_in" else "idle"
	end
	local function wait_for()
		local deadline = ui.now() + POLL_MAX_MS
		while ui.now() < deadline do
			if phase:peek() ~= "waiting" then return end
			ui.sleep(POLL_MS):await()
			if phase:peek() ~= "waiting" then return end
			local ok, status = pcall(function(): any return host.call("DshChatgptStatus", { provider = item.id }):await() end)
			if phase:peek() ~= "waiting" then return end
			if ok and type(status) == "table" then
				if (attempt_id and status.attemptId == attempt_id and status.outcome == "added") or (not attempt_id and status.running ~= true and status.authorized == true) then
					identity:set({ email = status.email, plan = status.plan })
					phase:set("signed_in")
					account_note:set(nil)
					refresh()
					chat.load_catalog()
					return
				elseif (attempt_id and status.attemptId == attempt_id and (status.outcome == "failed" or status.outcome == "cancelled")) or (not attempt_id and status.running ~= true) then
					phase:set(signed_phase())
					tried:set(true)
					note:set(ui.t("settings.providers.routes.sign_in.not_completed"):get())
					return
				end
			end
		end
		if phase:peek() == "waiting" then
			phase:set(signed_phase())
			tried:set(true)
			note:set(ui.t("settings.providers.routes.sign_in.not_completed"):get())
			host.call("DshChatgptCancel", { provider = item.id }):after(function() end, function() end)
		end
	end
	local poll = ui.async(wait_for)
	local function start()
		local p = phase:peek()
		if remote() or busy:peek() or (p ~= "idle" and p ~= "signed_in") or ((p == "signed_in" or item.id == "openai-codex") and not accounts_loaded:peek()) then return end
		phase:set("starting")
		note:set(nil)
		host.call("DshChatgptStart", { provider = item.id }):after(function(answer: any)
			if phase:peek() ~= "starting" then return end
			phase:set("waiting")
			attempt_id = if type(answer) == "table" and type(answer.attemptId) == "string" then answer.attemptId else nil
			local url = if type(answer) == "table" and type(answer.url) == "string" then answer.url else nil
			if url then pcall(host.invoke, "open_url", url) end
			poll()
		end, function(err: any)
			if phase:peek() == "starting" then
				phase:set(signed_phase())
				tried:set(true)
				note:set(ui.t("settings.providers.routes.sign_in.failed", { says = say(err) }):get())
			end
		end)
	end
	local function cancel()
		if phase:peek() ~= "waiting" then return end
		phase:set("cancelling")
		host.call("DshChatgptCancel", { provider = item.id }):after(function()
			phase:set(signed_phase())
			refresh()
		end, function() phase:set("waiting") end)
	end
	local function switch(a: any)
		if remote() or busy:peek() or a.active then return end
		busy:set(true)
		account_note:set(nil)
		host.call("DshChatgptSelect", { provider = item.id, accountId = a.id }):after(function()
			busy:set(false)
			refresh()
			chat.load_catalog()
		end, function(err: any)
			busy:set(false)
			account_note:set(ui.t("settings.providers.accounts.switch_failed", { says = say(err) }):get())
		end)
	end
	local function open_reset(a: any)
		if remote() or busy:peek() then return end
		local resets = {}
		for _, r in a.resets or {} do
			table.insert(resets, { id = r.id, title = r.title or ui.t("settings.providers.accounts.reset"):get(), expires = r.expires })
		end
		reset_account:set({ id = a.id, email = a.email or "ChatGPT account", resets = resets })
		reset_choice:set("")
		reset_open:set(true)
	end
	local function use_reset(a: any, credit: string)
		if busy:peek() or remote() then return end
		busy:set(true)
		serial += 1
		local for_credit = `{a.id}:{credit}`
		if reset_key_for ~= for_credit or not reset_key then
			reset_key_for = for_credit
			reset_key = `zgx-{ui.now()}-{serial}`
		end
		local key = reset_key
		reset_open:set(false)
		host.call("DshChatgptUseReset", { provider = item.id, accountId = a.id, creditId = credit, idempotencyKey = key }):after(function(answer: any)
			busy:set(false)
			local outcome = if type(answer) == "table" then answer.outcome else nil
			if outcome == "reset" or outcome == "nothing_to_reset" or outcome == "already_redeemed" or outcome == "no_credit" then reset_key = nil reset_key_for = nil end
			account_note:set(if outcome == "reset" then ui.t("settings.providers.accounts.reset_done"):get() elseif outcome == "nothing_to_reset" then ui.t("settings.providers.accounts.reset_not_needed"):get() elseif outcome == "already_redeemed" then ui.t("settings.providers.accounts.reset_used"):get() elseif outcome == "no_credit" then ui.t("settings.providers.accounts.no_credit"):get() else ui.t("settings.providers.accounts.unknown"):get())
			refresh()
		end, function(err: any)
			busy:set(false)
			account_note:set(ui.t("settings.providers.accounts.reset_failed", { says = say(err) }):get())
			refresh()
		end)
	end
	local function account_row(a: any): Node
		local email = if type(a.email) == "string" and a.email ~= "" then a.email else "ChatGPT account"
		local count = if type(a.resetCount) == "number" then a.resetCount else #(a.resets or {})
		return ui.column {
			w_full = true, id = `chatgpt-account-{item.id}-{a.id}`,
			ds.Settings.Row {
				compact = true, private_label = email ~= "ChatGPT account", label = email,
				description = if item.id == "openai-codex" and a.usageError then `{a.plan or "Signed in"} · {ui.t("settings.providers.accounts.usage_unavailable"):get()}` else a.plan or "Signed in", lead = ds.Avatar { seed = a.id or "openai", size = 28 },
				badge = if a.active then ds.Badge { ui.t("settings.providers.routes.in_use"), tone = "accent", size = "sm" } else nil,
				control = ui.row {
					gap = "$space.2", items = "center",
					ui.column {
						gap = "$space.1",
						if type(a.session) == "number" then usage_meter(ui.t("ds.provider_card.session"):get(), a.session) else nil,
						if type(a.week) == "number" then usage_meter(ui.t("ds.provider_card.week"):get(), a.week) else nil,
						if type(a.month) == "number" then usage_meter(ui.t("settings.providers.accounts.month"):get(), a.month) else nil,
					},
					if not a.active then ds.Button { ui.t("settings.providers.accounts.use_account"), tone = "secondary", size = "sm", disabled = function(): boolean return remote() or busy:get() end, on_click = function() switch(a) end } else nil,
				},
			},
			if count > 0 then ds.Settings.Row {
				compact = true, label = if count == 1 then ui.t("settings.providers.accounts.banked_one"):get() else ui.t("settings.providers.accounts.banked", { count = count }):get(),
				description = ui.t("settings.providers.accounts.banked_hint"), lead = route_lead("refresh"),
				control = if type(a.resets) == "table" and #a.resets > 0 and ((type(a.session) == "number" and a.session >= 1) or (type(a.week) == "number" and a.week >= 1) or (type(a.month) == "number" and a.month >= 1)) then ds.Button {
					ui.t("settings.providers.accounts.use_reset"), tone = "secondary", size = "sm", disabled = function(): boolean return remote() or busy:get() end,
					on_click = function() open_reset(a) end,
				} else nil,
			} else nil,
		}
	end
	return ui.column {
		w_full = true,
		ui.show(function(): boolean return not remote() end, ui.each(accounts, account_row, { key = function(a: any): string
			local credits = ""
			for _, r in a.resets or {} do credits ..= `:{r.id}:{tostring(r.title)}:{tostring(r.expires)}` end
			return `{a.id}:{tostring(a.active)}:{tostring(a.session)}:{tostring(a.week)}:{tostring(a.month)}:{tostring(a.resetCount)}:{tostring(a.usageError)}:{tostring(a.email)}:{tostring(a.plan)}{credits}`
		end, w_full = true })),
		ui.show(function(): boolean return remote() and (item.authorized or #accounts:get() > 0) end, ds.Settings.Row {
			compact = true, label = ui.t("settings.providers.accounts.remote"), lead = route_lead("user-rounded"),
		}),
		ui.show(function(): boolean return not remote() and #accounts:get() == 0 and phase:get() == "signed_in" end, ds.Settings.Row {
			compact = true, private_label = true,
			label = function(): string return identity:get().email or "ChatGPT account" end,
			description = function(): string return identity:get().plan or "Signed in" end,
			lead = ds.Avatar { seed = item.email or "openai", size = 28 },
		}),
		ui.show(function(): boolean return #accounts:get() > 0 or phase:get() == "signed_in" end, ds.Settings.Rule()),
		ui.show(function(): boolean return item.id == "openai-codex" and phase:get() == "idle" end, ds.Settings.Row {
			compact = true, label = ui.t("settings.providers.accounts.codex_title"),
			description = ui.t("settings.providers.accounts.codex_hint"), lead = route_lead("user-rounded"),
		}),
		ui.show(function(): boolean return phase:get() == "signed_in" end, ds.Settings.Row {
			compact = true, label = ui.t("settings.providers.accounts.add"), lead = route_lead("plus"),
			detail = function(): string? return note:get() or account_note:get() end,
			disabled = function(): boolean return remote() or busy:get() or not accounts_loaded:get() end,
			on_click = start,
		}),
		ui.show(function(): boolean return phase:get() ~= "signed_in" end, ds.Settings.Row {
			compact = true,
			label = function(): string return if #accounts:get() > 0 or phase:get() == "signed_in" then ui.t("settings.providers.accounts.add"):get() else ui.t("settings.providers.routes.sign_in.line"):get() end,
			lead = route_lead("plus"), detail = function(): string? return note:get() or account_note:get() end,
			control = ui.row {
				gap = "$space.2", items = "center",
				ui.show(function(): boolean return phase:get() == "waiting" end, ui.text { ui.t("settings.providers.routes.sign_in.waiting"), text_size = "$font.size.sm", color = "$text.secondary" }),
				ds.Button {
					function(): string
						return if phase:get() == "waiting" or phase:get() == "cancelling" then ui.t("settings.providers.routes.sign_in.cancel"):get()
							elseif #accounts:get() > 0 or phase:get() == "signed_in" then ui.t("settings.providers.accounts.add"):get()
							elseif tried:get() then ui.t("settings.providers.routes.sign_in.try_again"):get()
							else ui.t("settings.providers.routes.sign_in.start"):get()
					end,
					tone = "secondary", size = "sm", busy = function(): boolean return phase:get() == "starting" or phase:get() == "cancelling" end,
					disabled = function(): boolean return remote() or busy:get() or ((phase:get() == "signed_in" or item.id == "openai-codex") and not accounts_loaded:get()) or phase:get() == "starting" or phase:get() == "cancelling" end,
					on_key = function(e: any): boolean? if e.key == "Escape" and phase:peek() == "waiting" then cancel() return true end return nil end,
					on_click = function() if phase:peek() == "waiting" then cancel() else start() end end,
				},
			},

		}),
		ds.ProviderCard.ResetsDialog { open = reset_open, account = function(): any return reset_account:get() end, choice = reset_choice, on_use = use_reset },
	}
end

local oauth_row: (any, () -> boolean, () -> ()) -> Node

-- A route's row by the control it needs (the engine's own precedence:
-- its sign-in, then its key, then the account it's already part of).
local function route_row(item: any, remote: () -> boolean, creds: State<any>, on_authorized: () -> ()): Node
	-- (The block's title already names the provider: its row names the
	-- account or key it takes, as the upstream screen's lines do.)
	if item.chatgpt then
		return chatgpt_row(item, remote)
	elseif item.oauth then
		return oauth_row(item, remote, on_authorized)
	elseif item.key_ref then
		return key_row(item, remote, creds)
	elseif item.account then
		-- The account's own route: its plan's windows ride on the row.
		local function account_row(private: boolean): Node
			return ds.Settings.Row {
				compact = true,
				label = function(): string
					local a = account_for(item)
					return if a and type(a.email) == "string" and a.email ~= "" then a.email
						else (if item.id == "deepseek-official" then "DeepSeek · Official" elseif item.id == "deepseek-account" then "DeepSeek · Account" elseif item.id == "deepseek" then "DeepSeek · API" else item.name)
				end,
				private_label = private,
				lead = if account_for(item) then ds.Avatar { seed = item.id, size = 28 } else route_lead("user-rounded"),
				description = function(): string
					local a = account_for(item)
					return if a and type(a.plan) == "string" then a.plan else ""
				end,
				badge = ui.show(function(): boolean return item.in_use == true end,
					ds.Badge { ui.t("settings.providers.routes.in_use"), tone = "accent", size = "sm" }),
				control = ui.show(function(): boolean
					local a = account_for(item)
					return a ~= nil and type(a.session) == "number" and type(a.week) == "number" and a.key ~= true
				end, route_usage(item)),
			}
		end
		local function has_email(): boolean
			local a = account_for(item)
			return a ~= nil and type(a.email) == "string" and a.email ~= ""
		end
		return ui.column {
			w_full = true,
			ui.show(has_email, account_row(true)),
			ui.show(function(): boolean return not has_email() end, account_row(false)),
		}
	end
	return ds.Settings.Row {
		compact = true,
		label = ui.t("settings.providers.routes.nothing_line"),
		lead = route_lead("user-rounded"),
		description = ui.t("settings.providers.routes.nothing"),
	}
end

-- The provider's DSH authorization flow: browser notices and interactive
-- prompts stay attached to the request that started the flow.
oauth_row = function(item: any, remote: () -> boolean, on_authorized: () -> ()): Node
	return ui.component(function(): Node
			local phase = ui.use_state(if item.authorized then "signed_in" else "idle")
			local identity = ui.use_state({ email = item.email, accountName = item.accountName, plan = item.plan })
			local accounts = ui.use_state(item.accounts or {})
			local notice = ui.use_state(nil :: any)
			local prompt = ui.use_state(nil :: any)
			local answer = ui.use_state("")
			local selected = ui.use_state("")
			local busy = ui.use_state(false)
			local attempt = 0
			local opened_attempt: number? = nil
			local function apply_status(status: any): boolean
				if type(status) ~= "table" then return false end
				if type(status.accounts) == "table" then accounts:set(status.accounts) end
				if type(status.notice) == "table" then
					notice:set(status.notice)
					if type(status.notice.url) == "string" and opened_attempt ~= attempt then
						opened_attempt = attempt
						pcall(host.invoke, "open_url", status.notice.url)
					end
				end
				if type(status.prompt) == "table" then
					prompt:set(status.prompt)
					if status.prompt.kind == "select" and type(status.prompt.options) == "table" and status.prompt.options[1] then
						selected:set(status.prompt.options[1].id)
					end
				end
				if status.running ~= true and status.outcome == "failed" then
					phase:set(if #accounts:peek() > 0 then "signed_in" else "idle")
					notice:set({ message = status.error or "Sign-in did not complete." })
					return true
				elseif status.running ~= true and status.outcome == "cancelled" then
					phase:set(if #accounts:peek() > 0 then "signed_in" else "idle")
					return true
				end
				if status.authorized == true and status.running ~= true then
					phase:set("signed_in")
					identity:set({ email = status.email, accountName = status.accountName, plan = status.plan })
					prompt:set(nil)
					chat.load_catalog()
					if item.authorized ~= true then
						item.authorized = true
						on_authorized()
					end
					return true
				end
				return false
			end
			local function poll()
				local requested = attempt
				for _ = 1, math.floor(POLL_MAX_MS / POLL_MS) do
					if remote() or phase:peek() ~= "waiting" or attempt ~= requested then return end
					ui.sleep(POLL_MS):await()
					local ok, status = pcall(function(): any
						return host.call("DshProviderAuthStatus", { provider = item.id }):await()
					end)
					if attempt ~= requested then return end
					if ok and apply_status(status) then return end
				end
				if attempt == requested and phase:peek() == "waiting" then
					phase:set(if item.authorized then "signed_in" else "idle")
					host.call("DshProviderAuthCancel", { provider = item.id }):after(function() end, function() end)
				end
			end
			local polling = ui.async(poll)
			local function start()
				if remote() or busy:peek() or phase:peek() == "waiting" then return end
				busy:set(true)
				notice:set(nil)
				host.call("DshProviderAuthStart", { provider = item.id }):after(function()
					busy:set(false)
					attempt += 1
					phase:set("waiting")
					opened_attempt = nil
					polling()
				end, function(err: any)
					busy:set(false)
					notice:set({ message = say(err) })
				end)
			end
			local function cancel()
				if phase:peek() ~= "waiting" then return end
				attempt += 1
				phase:set(if #accounts:peek() > 0 then "signed_in" else "idle")
				host.call("DshProviderAuthCancel", { provider = item.id }):after(function() end, function() end)
			end
			local function use_account(account: any)
				if remote() or busy:peek() or account.active then return end
				busy:set(true)
				host.call("DshProviderAuthSelect", { provider = item.id, accountId = account.id }):after(function()
					busy:set(false)
					chat.load_catalog()
					host.call("DshProviderAuthStatus", { provider = item.id }):after(function(status: any)
						if type(status) == "table" and type(status.accounts) == "table" then accounts:set(status.accounts) end
						if type(status) == "table" then
							identity:set({ email = status.email, accountName = status.accountName, plan = status.plan })
							item.authorized = status.authorized == true
						end
						on_authorized()
					end, function(err: any) notice:set({ message = say(err) }) end)
				end, function(err: any)
					busy:set(false)
					notice:set({ message = say(err) })
				end)
			end
			local function respond()
				local p = prompt:peek()
				if type(p) ~= "table" then return end
				local value = if p.kind == "select" then selected:peek() else answer:peek()
				host.call("DshProviderAuthRespond", { provider = item.id, promptId = p.id, value = value }):after(function()
					answer:set("")
					prompt:set(nil)
				end, function(err: any) notice:set({ message = say(err) }) end)
			end
			local function prompt_control(): Node
				local p = prompt:get()
				if type(p) ~= "table" then return ui.column {} end
				if p.kind == "select" and type(p.options) == "table" then
					local options = {}
					for _, option in p.options do
						table.insert(options, { value = option.id, label = option.label })
					end
					return ui.row { gap = "$space.2", items = "center",
						ds.Select { value = selected, name = p.message or ui.t("settings.providers.routes.oauth.prompt"), options = options, w = 200 },
						ds.Button { ui.t("settings.providers.routes.oauth.continue"), size = "sm", on_click = respond },
					}
				end
				return ui.row { gap = "$space.2", items = "center",
					ds.TextField { value = answer, grow = 1, size = "sm", password = p.kind == "secret",
						name = p.message or ui.t("settings.providers.routes.oauth.prompt"), placeholder = p.placeholder or ui.t("settings.providers.routes.oauth.placeholder"), on_commit = respond },
					ds.Button { ui.t("settings.providers.routes.oauth.continue"), size = "sm", on_click = respond },
				}
			end
			local function provider_account(account: any): Node
				local meters = {}
				if type(account.session) == "number" then table.insert(meters, usage_meter(ui.t("ds.provider_card.session"):get(), account.session)) end
				if type(account.week) == "number" then table.insert(meters, usage_meter(ui.t("ds.provider_card.week"):get(), account.week)) end
				if type(account.month) == "number" then table.insert(meters, usage_meter(ui.t("settings.providers.accounts.month"):get(), account.month)) end
				if type(account.vibe_usage) == "number" then table.insert(meters, usage_meter(ui.t("settings.providers.accounts.vibe_usage"):get(), account.vibe_usage)) end
				return ds.Settings.Row {
					compact = true, private_label = account.email ~= nil,
					label = account.email or account.accountName or ui.t("settings.providers.routes.oauth.signed_in"),
					description = account.plan or ui.t("settings.providers.routes.oauth.signed_in_description", { name = item.name }),
					lead = ds.Avatar { seed = account.email or account.accountName or account.id, size = 28 },
					badge = ui.show(function(): boolean return account.active == true end,
						ds.Badge { ui.t("settings.providers.routes.in_use"), tone = "accent", size = "sm" }),
					control = ui.row {
						gap = "$space.2", items = "center",
						if #meters > 0 then ui.column { gap = "$space.1", table.unpack(meters) } else nil,
						if account.active then nil else ds.Button {
							ui.t("settings.providers.accounts.use_account"), size = "sm", tone = "secondary", busy = busy,
							disabled = function(): boolean return remote() or busy:get() end,
							on_click = function() use_account(account) end,
						},
					},
				}
			end
			return ui.column { w_full = true,
				ui.each(accounts, provider_account, { key = function(account: any): string return account.id end, w_full = true }),
				ds.Settings.Row {
					compact = true,
					label = function(): string
						return if #accounts:get() > 0 then ui.t("settings.providers.accounts.add"):get()
							else ui.t("settings.providers.routes.sign_in.line"):get()
					end,
					lead = route_lead("plus"),
					detail = function(): string?
						local n = notice:get()
						return if type(n) == "table" and type(n.message) == "string" then n.message else nil
					end,
					disabled = function(): boolean return remote() or busy:get() end,
					on_click = function() if phase:peek() == "waiting" then cancel() else start() end end,
					control = ui.show(function(): boolean return phase:get() == "waiting" end, ds.Button {
						ui.t("settings.providers.routes.sign_in.cancel"),
						size = "sm", tone = "secondary", busy = busy,
						disabled = function(): boolean return remote() or busy:get() end,
						on_click = cancel,
					}),
				},
				ui.show(function(): boolean return prompt:get() ~= nil end, ui.row { w_full = true, pl = ui.raw(44, "authorization prompt indent"), prompt_control() }),
			}
	end)()
end

-- The gateway's answer, as the page needs it: the parsed routes, which of
-- their credential refs are saved, and whether the chatgpt grant is there
-- already (its status asked once at load; the wait polls it later).
local function load(): any
	local answer = host.call("DshProviders", {}):await()
	local entries = if type(answer) == "table" and type(answer.providers) == "table" then answer.providers else answer
	local routes: { any } = {}
	local refs: { string } = {}
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
	-- The route DSH runs by default ("In use", as the upstream screen says).
	local in_use: string? = nil
	local ok_settings, settings = pcall(function(): any
		return host.call("DshSettings", {}):await()
	end)
	if ok_settings and type(settings) == "table" and type(settings.namespaces) == "table" then
		for _, entry in settings.namespaces do
			if type(entry) == "table" and entry.ns == "agent-default-model" and type(entry.value) == "table" then
				in_use = if type(entry.value.provider) == "string" then entry.value.provider else nil
			end
		end
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
	for i, route in routes do
		route.order = i
		route.in_use = route.id == in_use
		if route.chatgpt then
			local ok, status = pcall(function(): any
				return host.call("DshChatgptStatus", { provider = route.id }):await()
			end)
			route.authorized = ok and type(status) == "table" and (route.id == "openai" or status.provider == route.id) and status.authorized == true
			route.email = if route.authorized then status.email else nil
			route.plan = if route.authorized then status.plan else nil
			local ok_accounts, account_state = pcall(function(): any
				return host.call("DshChatgptAccounts", { provider = route.id }):await()
			end)
			route.accounts = if ok_accounts and type(account_state) == "table" and type(account_state.accounts) == "table" then account_state.accounts else {}
		elseif route.oauth then
			local ok, status = pcall(function(): any
				return host.call("DshProviderAuthStatus", { provider = route.id }):await()
			end)
			route.authorized = ok and type(status) == "table" and status.authorized == true
			route.auth_supported = ok and type(status) == "table" and status.supported == true
			route.email = if route.authorized then status.email else nil
			route.accountName = if route.authorized then status.accountName else nil
			route.plan = if route.authorized then status.plan else nil
			route.accounts = if ok and type(status) == "table" and type(status.accounts) == "table" then status.accounts else {}
		end
	end
	return { routes = routes, creds = creds, counts = counts, in_use = in_use }
end

-- The header switch controls whether this route's models appear in the picker.
local function model_switch(item: any): Node
	return ui.component(function(): Node
		local on = ui.use_state(ModelVisibility.showing(item.id))
		return ds.Switch {
			compact = true,
			value = on,
			label = ui.t("settings.providers.models.every", { name = item.name }),
			on_change = function(value: boolean) ModelVisibility.set_showing(item.id, value) end,
		}
	end)()
end

-- The agents' accounts, as the upstream screen lists them: who is signed in,
-- what the plan is using, and which one runs.
function M.body(): Node
	return ui.component(function(): Node
		-- The catalog and what it needs, loaded once a visit (Try again
		-- loads again); the refs' saved state beside it, rows writing
		-- theirs when a key lands.
		local res = ui.resource(load)
		-- The accounts section's data, asked as the page opens (the visit
		-- hook alone misses a page restored straight into view).
		ui.effect(function()
			agents.load()
		end)
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
		-- The providers this device actually has, as the upstream screen lists
		-- them; the catalog's long tail (a route per API host, most with no
		-- key) waits behind "Show all".
		local show_all = ui.use_state(false)
		local function ready(route: any, saved: any): boolean
			if route.chatgpt then
				return route.authorized == true
			end
			if route.oauth then return route.authorized == true or (route.key_ref ~= nil and configured_in(saved, route.key_ref)) end
			return route.key_ref ~= nil and configured_in(saved, route.key_ref)
		end
		local catalog = ui.computed(function(): { any }
			local d = res.data:get()
			local routes = if d then table.clone(d.routes) else {}
			local saved = creds:get()
			local all = show_all:get()
			local shown = {}
			for _, route in routes do
				if all or ready(route, saved) or route.id == "openai-codex" or route.account or route.oauth or string.find(route.id, "deepseek", 1, true) == 1 then
					table.insert(shown, route)
				end
			end
			table.sort(shown, function(a: any, b: any): boolean
				local a_ready, b_ready = ready(a, saved), ready(b, saved)
				if a_ready ~= b_ready then
					return a_ready
				end
				return a.order < b.order
			end)
			local groups, deepseek = {}, nil
			for _, route in shown do
				if string.find(route.id, "deepseek", 1, true) == 1 then
					if not deepseek then
						deepseek = { id = "deepseek-group", name = "DeepSeek", routes = {} }
						table.insert(groups, deepseek)
					end
					table.insert(deepseek.routes, route)
				else
					table.insert(groups, { id = route.id, name = route.name, routes = { route } })
				end
			end
			return groups
		end)
		-- How many the catalog holds beyond what is shown.
		local hidden_count = ui.computed(function(): number
			local d = res.data:get()
			if not d then
				return 0
			end
			local saved = creds:get()
			local n = 0
			for _, route in d.routes do
				if not ready(route, saved) and route.id ~= "openai-codex" and not route.account and not route.oauth and string.find(route.id, "deepseek", 1, true) ~= 1 then
					n += 1
				end
			end
			return n
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
			ui.each(catalog, function(group: any): Node
				return ui.component(function(): Node
				local menu_open = ui.use_state(false)
				local removing = ui.use_state(false)
				local remove_error = ui.use_state(nil :: string?)
				local item = group.routes[1]
				local function active_account(): (any?, any?)
					for _, route in group.routes do
						if route.oauth or route.chatgpt then
							for _, account in route.accounts or {} do
								if account.active then return route, account end
							end
						end
					end
					return nil, nil
				end
				local trigger = ds.IconButton {
					icon = "more-horizontal",
					label = ui.t("settings.providers.accounts.options", { name = group.name }),
					size = "sm", tone = "quiet", lit = menu_open,
					on_click = function() menu_open:set(true) end,
				}
				local account_menu = if item.oauth or item.chatgpt then ui.popover(Menu.panel {
					open = menu_open, anchor = trigger,
					placement = { side = "bottom", align = "end", offset = 4 },
					w = "$zeron.menu.width",
					build = function()
						local route, account = active_account()
						return Menu.Panel {
							w = "$zeron.menu.width",
							Menu.Group {
								heading = ui.t("settings.providers.accounts.title"),
								Menu.Row {
									icon = "trash-bin-minimalistic",
									label = ui.t("settings.providers.accounts.remove"),
									danger = true,
									disabled = account == nil or remote() or removing:get(),
									on_click = function()
										if not route or not account or removing:peek() then return end
										removing:set(true)
										remove_error:set(nil)
										menu_open:set(false)
										local action = if route.chatgpt then "DshChatgptRemoveAccount" else "DshProviderAuthRemove"
										host.call(action, { provider = route.id, accountId = account.id }):after(function()
											removing:set(false)
											res.reload()
											chat.load_catalog()
										end, function(err: any)
											removing:set(false)
											remove_error:set(ui.t("settings.providers.accounts.remove_failed", { says = say(err) }):get())
										end)
								end,
							},
							},
						}
					end,
				}) else nil
				local d = res.data:peek()
				local count = 0
				local model_ids = {}
				for _, route in group.routes do
					local n = if d and d.counts then d.counts[route.id] or 0 else 0
					count += n
					if n > 0 then table.insert(model_ids, route.id) end
				end
				local children = {}
				for _, route in group.routes do
					table.insert(children, ds.Settings.Rule())

					table.insert(children, route_row(route, remote, creds, function() res.reload() end))
				end
				return ds.Settings.Group {
					ui.column {
						w_full = true, id = `provider-card-{group.id}`,
						ui.row {
							w_full = true, min_h = ui.raw(54, "the provider card header"),
							px = ui.raw(15, "the provider card header inset"), gap = "$space.3", items = "center",
							ds.AgentMark {
								label = group.name,
								mark = if item.id == "openai-codex" then "openai" elseif item.id == "opencode" or item.id == "opencode-go" then "opencode" else nil,
								src = if MARKS[item.id] then `assets/icons/{MARKS[item.id]}.svg` else nil,
								seed = `own.{item.id}`, size = 28,
							},
							ui.column {
								grow = 1, min_w = 0, gap = "$zeron.space.hair",
								ui.text { group.name, heading = 2, text_size = "$font.size.sm", line_height = "$font.line.sm", weight = "$font.weight.medium", color = "$text.primary" },
								if count and count > 0 then ui.text {
									if count == 1 then ui.t("settings.providers.models.count_one") else ui.t("settings.providers.models.count", { count = count }),
									text_size = "$font.size.xs", color = "$text.secondary",
								} else nil,
							},
							if account_menu then account_menu else nil,
							if #model_ids == 1 then model_switch({ id = model_ids[1], name = group.name })
							elseif #model_ids > 1 then ui.component(function(): Node
								local all_on = true
								for _, id in model_ids do if not ModelVisibility.showing(id) then all_on = false end end
								local on = ui.use_state(all_on)
								return ds.Switch {
									value = on, compact = true, label = ui.t("settings.providers.models.every", { name = group.name }),
									on_change = function(value: boolean)
										for _, id in model_ids do ModelVisibility.set_showing(id, value) end
									end,
								}
							end)() else nil,
						},
							ui.show(function(): boolean return remove_error:get() ~= nil end, ds.Hint { function(): string return remove_error:get() or "" end }),
							table.unpack(children),
					},
					}
				end)()
			end, { key = function(group: any): string return group.id end, w_full = true, gap = ui.raw(14, "the provider cards spacing") }),
			-- The rest of the catalog, on request.
			ui.show(function(): boolean
				return hidden_count:get() > 0 or show_all:get()
			end, ui.row {
				w_full = true,
				px = ds.Settings.inset(),
				ds.Button {
					function(): string
						if show_all:get() then
							return ui.t("settings.providers.routes.show_fewer"):get()
						end
						return ui.t("settings.providers.routes.show_all", { count = hidden_count:get() }):get()
					end,
					tone = "secondary",
					size = "sm",
					on_click = function()
						show_all:set(not show_all:peek())
					end,
				},
			}),
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
		}
	end)({})
end

function M.view(): Node
	visits.once("providers", function() agents.load() end)
	return ds.Settings.Page {
		title = "Providers",
		measure = ui.raw(552, "the compact provider card measure"),
		compact = true,
		description = ui.t("settings.providers.routes.page_description"),
		trail = devices.picker(function() return agents.now:get().target or here() end, function(id)
			agents.target(if id == here() then nil else id)
		end, function()
			return if cloud.device() then { { value = "cloud", label = "Cloud", icon = "cloud" } } else {}
		end, true),
		trail_top = true,
		M.body(),
	}
end

return M
