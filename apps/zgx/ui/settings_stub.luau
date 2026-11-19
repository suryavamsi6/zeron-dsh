-- Settings' host calls, answered here as the host would (no engine or edge
-- in a test host): your profile and card, your organization, its members
-- and invitations, this computer's engine settings and banners. The shell's
-- fixture requires it, so Settings' pages build headless; the tests drive
-- it (tests/settings_in_place.test.luau).
--
-- What was asked, in order, is `settings_stub_calls` (a state, "name" or
-- "name:detail" each), and the plugin operations in
-- `settings_stub_dsh_plugin_calls` (a state, `{ op, value }` each).
-- `ui.command`s change the data as the edge would on its own, or make the
-- next change fail:
--   settings_stub.sign_in          signed in as Ada, in org_1 (else signed out)
--   settings_stub.restoring        the engine hasn't answered yet
--   settings_stub.signed_out       the engine says signed out
--   settings_stub.open_account     a way in to `settings/account` (a link)
--   settings_stub.member_joins     someone accepts an invitation
--   settings_stub.refuse_next      the next change is refused ("refused")
--   settings_stub.not_admin        you're a member, not an admin, from now on
--   settings_stub.signed_in_again  the browser sign-in a delete waits on lands

local host = require("@app/screens/settings/host")
local api = require("@account/api")
local auth = require("@account/auth")

local M = {}

local calls = ui.state("settings_stub_calls", {} :: { string })
local refuse = ui.state("settings_stub_refuse", false)
local role = ui.state("settings_stub_role", "admin")
local about_next = ui.state("settings_stub_about_next", "ok")
local delete_word = ui.state("settings_stub_delete_word", "ok")
-- What a delete after signing in again answers: "ok", "wait" (until
-- `settings_stub.signed_in_again`, or cancelled), or a refusal.
local step_up_word = ui.state("settings_stub_step_up_word", "ok")
-- The account's signing keys: "some" (this computer's, one new on another
-- device, one revoked; listed out of order) or "none".
local keys_mode = ui.state("settings_stub_keys", "some")
-- What `DshPlugins` answers: nil, the two bundles below, or what a test
-- sets (`{ bundles = …, pnpm = … }`).
local dsh_plugins_answer = ui.state("settings_stub_dsh_plugins", nil :: any)
-- What `DshPluginAdd` answers: nil (installed), "pending" (never settles),
-- another string (the run refused that way), or a table a test sets
-- (`{ ok = false, refusal = …, log = … }`). `DshPluginRemove` answers the
-- same way from `settings_stub_dsh_plugin_remove`.
local dsh_plugin_add = ui.state("settings_stub_dsh_plugin_add", nil :: any)
local dsh_plugin_remove = ui.state("settings_stub_dsh_plugin_remove", nil :: any)
-- What `DshPluginRows` answers: nil, `row_store` below by bundle `name`,
-- or what a test sets — a `{ rows = … }` answered for any name, a
-- `{ ["bundle-name"] = { rows = … } }` answered for its own.
local dsh_plugin_rows = ui.state("settings_stub_dsh_plugin_rows", nil :: any)
-- What `DshPluginSetRow` answers: nil (the write lands in `row_store`
-- and the owning bundle's refreshed rows answer), "pending" (never
-- settles), another string (refused that way), or a table a test sets.
local dsh_plugin_set_row = ui.state("settings_stub_dsh_plugin_set_row", nil :: any)
-- What `DshPluginIndex` answers: nil (the four-entry index below),
-- "pending" (never settles), another string (the fetch refused that
-- way — a malformed index's words are one), or a table a test sets
-- (`{title, plugins}`).
local dsh_plugin_index = ui.state("settings_stub_dsh_plugin_index", nil :: any)
-- What `Schedules` answers: nil (the `task_store` below), or a
-- `{ tasks = … }` a test sets. Each write's answer is its own state:
-- `settings_stub_schedule_save`, `_delete` and `_run` take nil (the op
-- lands as the engine's would), "pending" (never settles), another
-- string (refused that way — the engine's words for a rule with no run
-- ahead), or a table (its answer). The ops asked for, in order, are in
-- `settings_stub_schedule_calls` (`{ op = "save"|"delete"|"run", … }`).
local schedule_tasks = ui.state("settings_stub_schedule_tasks", nil :: any)
local schedule_save = ui.state("settings_stub_schedule_save", nil :: any)
local schedule_delete = ui.state("settings_stub_schedule_delete", nil :: any)
local schedule_run = ui.state("settings_stub_schedule_run", nil :: any)
local schedule_calls = ui.state("settings_stub_schedule_calls", {} :: { any })
local signing_keys = {
	{ key = "k_dev", fingerprint = "9f3a 21c0 77be 04d1 aa10 bb20 cc30 dd40", device_name = "dev", device_id = "d_dev", this_device = false, created_at = 1793600000 },
	{ key = "k_old", fingerprint = "0b0b 1c1c 2d2d 3e3e 4f4f 5a5a 6b6b 7c7c", device_name = "old-laptop", device_id = "d_old", this_device = false, created_at = 1780000000, revoked_at = 1785000000 },
	{ key = "k_mac", fingerprint = "a1b2 c3d4 e5f6 0718 2930 4a5b 6c7d 8e9f", device_name = "Ada's MacBook Pro", device_id = "d_mac", this_device = true, created_at = 1790000000 },
}
-- That wait, while it's open.
local signing_in: { ok: any, bad: any }? = nil
local function waiting(): any
	local p = {}
	function p.after(_self: any, ok: ((any) -> ())?, bad: ((any) -> ())?)
		signing_in = { ok = ok, bad = bad }
	end
	return p
end
local function signed_in_again(word: string?)
	local w = signing_in
	signing_in = nil
	if not w then
		return
	end
	if word then
		if w.bad then
			w.bad({ message = word })
		end
	elseif w.ok then
		w.ok(true)
	end
end
-- What the next photo upload answers ("ok", or a refusal like "too_large").
local photo_word = ui.state("settings_stub_photo_word", "ok")
-- The last square a photo was cut at, for a test to read.
local last_crop = ui.state("settings_stub_last_crop", {} :: any)
local photo_n = 0

local function log(entry: string)
	calls:push(entry)
end

-- A promise already settled, as the host's settle: `:after` at once.
local function settled(value: any, word: string?): any
	local p = { done = true, value = if word then nil else value, error = if word then { message = word } else nil }
	function p.after(self: any, ok: ((any) -> ())?, bad: ((any) -> ())?)
		if word then
			if bad then
				bad({ message = word })
			end
		elseif ok then
			ok(value)
		end
	end
	-- A resource's `load` awaits it (`ui.call(...):await()`), as the host's
	-- own promise is awaited.
	function p.await(self: any): any
		if word then
			error({ message = word })
		end
		return value
	end
	return p
end

-- A change: refused if asked to be, else `value`.
local function change(value: any): any
	if refuse:peek() then
		refuse:set(false)
		return settled(nil, "refused")
	end
	return settled(value)
end

local me = { first_name = "Ada", last_name = "Lovelace", name = "Ada Lovelace", email = "ada@example.net" }
local card: api.Card = {
	username = "ada",
	first_name = "Ada",
	number = 1815,
	joined_on = "2026-10-31",
	badges = {},
	scene = 0,
	holo = "glyph",
	sky = 0,
}
local members = {
	{ id = "om_ada", name = "Ada Lovelace", email = "ada@example.net", role = "admin", you = true, username = "ada" },
	{ id = "om_grace", name = "Grace Hopper", email = "grace@example.net", role = "member" },
	{ id = "om_alan", name = "Alan Turing", email = "alan@example.net", role = "member" },
}
local invitations = {
	{ id = "inv_1", email = "edsger@example.net", state = "pending", created_at = "2026-10-30T10:00:00Z" },
	{ id = "inv_2", email = "barbara@example.net", state = "expired", created_at = "2026-10-01T10:00:00Z" },
}
local engine: { [string]: any } = {
	GetCommitCredit = { enabled = false, cloud = false },
	GetWorktreeSettings = {
		settings = { useCustomDirectory = false },
		defaultDirectory = "~/.zeron/worktrees",
		effectiveDirectory = "~/.zeron/worktrees",
	},
	GetTitleSettings = {},
	GetNativeSubagents = { switchable = { "claude-code" }, off = {} },
}
local banners = { desktop = true, background_only = true, agent_updates = true }

local function find(list: { any }, id: string): number?
	for i, item in list do
		if item.id == id then
			return i
		end
	end
	return nil
end

-- Which key refs the stub has stored, and the ChatGPT sign-in's state.
local keys: { [string]: boolean } = {}
local chatgpt = { running = false, authorized = false }
local chatgpt_fixture = ui.state("settings_stub_chatgpt", nil :: any)
local chatgpt_accounts = ui.state("settings_stub_chatgpt_accounts", nil :: any)
local codex_fixture = ui.state("settings_stub_codex", nil :: any)
local codex_accounts = ui.state("settings_stub_codex_accounts", nil :: any)
local unscoped_chatgpt = ui.state("settings_stub_chatgpt_unscoped", false)
-- The plugin operations asked for, in order: `{ op = "add", value = "…" }`
-- (an add's `allowBuild` kept when the page sent one), `{ op = "remove",
-- value = "…" }`, or `{ op = "set_row", value = "…", disabled = … }` (the
-- write's whole `{id, disabled}` — an argument dropped on the way fails
-- what a test reads).
local plugin_calls = ui.state("settings_stub_dsh_plugin_calls", {} :: { any })
-- The patch rows each bundle answers with, as a profile's manifests and
-- cordis.patch.yml would say them: `dsh-hello-plugin` has an on row, an
-- off-by-default one, and one it can't switch (`!!js`); `left-pad` one.
local row_store: { [string]: { any } } = {
	["dsh-hello-plugin"] = {
		{ id = "hello", title = "dsh-hello-plugin", disabled = false, overridden = false, editable = true },
		{ id = "side", disabled = true, overridden = false, editable = true },
		{
			id = "computed",
			disabled = false,
			overridden = false,
			editable = false,
			note = "it carries a !!js expression, which can't be switched safely",
		},
	},
	["left-pad"] = {
		{ id = "left-pad", disabled = false, overridden = false, editable = true },
	},
}

-- A plugin run's answer: "pending" never settles, a table is it, another
-- string the way it refused, nil the run done.
local function plugin_run(over: any): any
	if over == "pending" then
		-- (Still on its way: never settles.)
		return { done = false, after = function() end }
	elseif type(over) == "table" then
		return settled(over)
	elseif type(over) == "string" then
		return settled(nil, over)
	end
	return settled({ ok = true, exitCode = 0, log = "", refusal = nil })
end

-- The engine's scheduled tasks (crates/engine/src/schedules.rs), epoch
-- ms as it keeps them: one active daily in the chat open, one paused
-- weekly making a new chat once (its `session` the one it made).
local TASK_NOW = math.floor(os.time() * 1000)
local task_store: { any } = {
	{
		id = "task_daily",
		title = "Standup summary",
		prompt = "Summarize what landed yesterday.",
		rrule = "DTSTART:20261101T090000Z\nRRULE:FREQ=DAILY;BYHOUR=9;BYMINUTE=0",
		target = { kind = "session", session = "a1" },
		active = true,
		nextAt = TASK_NOW + 26 * 3600 * 1000,
		lastRunAt = TASK_NOW - 22 * 3600 * 1000,
		updatedAt = TASK_NOW - 24 * 3600 * 1000,
	},
	{
		id = "task_paused",
		title = "Weekly digest",
		prompt = "Write the week's notes.",
		rrule = "DTSTART:20261102T073000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO;BYHOUR=7;BYMINUTE=30",
		target = { kind = "new_once", cwd = "/work/atlas", session = "a2" },
		active = false,
		nextAt = nil,
		lastRunAt = TASK_NOW - 7 * 86400 * 1000,
		updatedAt = TASK_NOW - 8 * 86400 * 1000,
	},
}

-- A schedule op's override answer, as `plugin_run` does a plugin's:
-- nil means it lands.
local function schedule_over(over: any): any
	if over == "pending" then
		return { done = false, after = function() end }
	elseif type(over) == "table" then
		return settled(over)
	elseif type(over) == "string" then
		return settled(nil, over)
	end
	return nil
end

local answers: { [string]: (arg: any) -> any } = {
	-- The provider page's gateway calls (crates/engine/src/dsh_gateway.rs):
	-- two routes, one that takes a key and the ChatGPT one, so a test can
	-- walk both controls without an engine or a browser.
	["DshProviders"] = function()
		return settled({
			{
				provider = "deepseek-official",
				displayName = "DeepSeek",
				settingsNs = "llm-deepseek",
			},
			{
				provider = "deepseek",
				displayName = "DeepSeek",
				settingsNs = "llm-pi-ai",
				nativeAuth = { env = { "DEEPSEEK_API_KEY" }, oauth = false },
			},
			{
				provider = "opencode-go",
				displayName = "OpenCode Go",
				settingsNs = "llm-pi-ai",
				nativeAuth = { env = { "OPENCODE_API_KEY" }, oauth = false },
			},
			{
				provider = "openai",
				displayName = "OpenAI",
				settingsNs = "llm-pi-ai",
				nativeAuth = { env = {}, oauth = true },
			},
			-- The catalog names these by their route id; the page renames
			-- them while keeping the separate Codex route.
			{
				provider = "openai-codex",
				displayName = "openai-codex",
				settingsNs = "llm-pi-ai",
				nativeAuth = { env = {}, oauth = true },
			},
			{
				provider = "opencode",
				displayName = "opencode",
				settingsNs = "llm-pi-ai",
				nativeAuth = { env = { "OPENCODE_API_KEY" }, oauth = false },
			},
		})
	end,
	-- The default model's route, for the "In use" marker.
	["DshSettings"] = function()
		return settled({
			namespaces = {
				{ ns = "agent-default-model", value = { provider = "opencode-go", model = "gpt-6-luna" } },
			},
		})
	end,
	-- The routes' models, for the Models section's counts.
	["ListModels"] = function()
		return settled({
			{ id = "gpt-6-luna", label = "GPT-6 Luna", provider = { id = "openai", name = "OpenAI" }, reasoningLevels = {} },
			{ id = "gpt-6-luna", label = "GPT-6 Luna", provider = { id = "opencode-go", name = "OpenCode Go" }, reasoningLevels = {} },
			{ id = "deepseek-flash", label = "DeepSeek V4.1 Flash", reasoningLevels = {} },
		})
	end,
	["DshCredentials"] = function(arg: any)
		local out = {}
		for _, ref in (if type(arg) == "table" and type(arg.refs) == "table" then arg.refs else {}) do
			out[ref] = { configured = keys[ref] == true }
		end
		return settled(out)
	end,
	["DshSetCredential"] = function(arg: any)
		if refuse:peek() then
			return failed("refused")
		end
		if type(arg) ~= "table" or type(arg.ref) ~= "string" or arg.ref == "" then
			return failed("no ref")
		end
		keys[arg.ref] = true
		return settled(nil)
	end,
	["DshMutateSettings"] = function(arg: any)
		if refuse:peek() then
			return failed("refused")
		end
		if type(arg) ~= "table" or type(arg.ops) ~= "table" then
			return failed("no ops")
		end
		return settled(nil)
	end,
	-- The dsh bundles in this computer's `sdk` profile, and the two
	-- operations that change it (crates/engine/src/dsh_plugins.rs): one
	-- active bundle with a description, one inactive without.
	["DshPlugins"] = function()
		local over = dsh_plugins_answer:peek()
		if over ~= nil then
			return settled(over)
		end
		return settled({
			bundles = {
				{
					name = "dsh-hello-plugin",
					spec = "^0.1.0",
					active = true,
					version = "0.1.0",
					enginesDsh = ">=0.2.0 <0.3.0",
					description = "Says hello",
				},
				{ name = "left-pad", spec = "^1.3.0", active = false, version = "1.3.0" },
			},
			pnpm = true,
		})
	end,
	["DshPluginAdd"] = function(arg: any)
		plugin_calls:push({ op = "add", value = arg.spec, allowBuild = arg.allowBuild })
		return plugin_run(dsh_plugin_add:peek())
	end,
	["DshPluginRemove"] = function(arg: any)
		plugin_calls:push({ op = "remove", value = arg.name })
		return plugin_run(dsh_plugin_remove:peek())
	end,
	-- A plugin index's entries for the page's Discover section, as
	-- crates/engine/src/dsh_plugin_index.rs reports them (`{title,
	-- plugins}`): an npm entry at its recommended version, a git-sourced
	-- one it can't install, one no listed version is compatible for, and
	-- one whose engines.dsh couldn't be read.
	["DshPluginIndex"] = function(_arg: any)
		local over = dsh_plugin_index:peek()
		if over == "pending" then
			-- (Still on its way: never settles.)
			return { done = false, after = function() end }
		elseif type(over) == "table" then
			return settled(over)
		elseif type(over) == "string" then
			return settled(nil, over)
		end
		return settled({
			title = "Team plugins",
			plugins = {
				{
					name = "dsh-plugin-foo",
					title = "Foo tools",
					description = "Foo's own tools",
					author = "Ada Dev",
					version = "1.1.0",
					source = "npm",
					installable = true,
					note = nil,
				},
				{
					name = "dsh-plugin-bar",
					title = "dsh-plugin-bar",
					description = nil,
					author = "Grace",
					version = "0.4.0",
					source = "git",
					installable = false,
					note = "Installs from a git repository — add it by its URL.",
				},
				{
					name = "dsh-plugin-broken",
					title = "Broken",
					description = nil,
					author = nil,
					version = nil,
					source = nil,
					installable = false,
					note = "No listed version is compatible with this device.",
				},
				{
					name = "dsh-plugin-mystery",
					title = "dsh-plugin-mystery",
					description = nil,
					author = nil,
					version = "1.2.3",
					source = "npm",
					installable = true,
					note = "Its engines.dsh couldn't be read — compatibility is unknown.",
				},
			},
		})
	end,
	-- One bundle's patch rows (`{name}` -> `{rows: […]}`), and a row's
	-- switch (`{id, disabled}` -> the owning bundle's refreshed rows),
	-- as crates/engine/src/dsh_plugins.rs answers them.
	["DshPluginRows"] = function(arg: any)
		local over = dsh_plugin_rows:peek()
		if type(over) == "table" then
			if type(over.rows) == "table" then
				return settled(over)
			end
			local named = over[if type(arg) == "table" then arg.name else nil]
			if named ~= nil then
				return settled(named)
			end
		end
		return settled({ rows = row_store[if type(arg) == "table" then arg.name else ""] or {} })
	end,
	["DshPluginSetRow"] = function(arg: any)
		plugin_calls:push({ op = "set_row", value = if type(arg) == "table" then arg.id else nil, disabled = if type(arg) == "table" then arg.disabled else nil })
		local over = dsh_plugin_set_row:peek()
		if over == "pending" then
			-- (Still on its way: never settles.)
			return { done = false, after = function() end }
		elseif type(over) == "table" then
			return settled(over)
		elseif type(over) == "string" then
			return settled(nil, over)
		end
		-- The write lands as the profile's override would, and the owning
		-- bundle's rows come back refreshed.
		if type(arg) == "table" and type(arg.id) == "string" and type(arg.disabled) == "boolean" then
			for _, list in row_store do
				for i, r in list do
					if r.id == arg.id then
						local next = table.clone(list)
						local row = table.clone(r)
						row.disabled = arg.disabled
						row.overridden = true
						next[i] = row
						list[i] = row
						return settled({ rows = next })
					end
				end
			end
		end
		return settled({ rows = {} })
	end,
	-- The scheduled tasks (crates/engine/src/schedules.rs): `Schedules`
	-- lists `task_store` (or a test's own), `ScheduleSave` replaces by
	-- `id` — an empty one creating — recomputing `nextAt` (none while
	-- paused) and `updatedAt` as the engine does, `ScheduleDelete`
	-- answers `{deleted}`, `ScheduleRunNow` fires it once (its rule and
	-- pause as they were; a `new_once`/`new` target's `session` becomes
	-- the chat it made).
	["Schedules"] = function()
		local over = schedule_tasks:peek()
		if over ~= nil then
			return settled(over)
		end
		local out = {}
		for _, t in task_store do
			table.insert(out, table.clone(t))
		end
		return settled({ tasks = out })
	end,
	["ScheduleSave"] = function(arg: any)
		local t = if type(arg) == "table" then arg.task else nil
		schedule_calls:push({ op = "save", task = t })
		local over = schedule_over(schedule_save:peek())
		if over ~= nil then
			return over
		end
		if type(t) ~= "table" or type(t.title) ~= "string" or type(t.prompt) ~= "string" or type(t.rrule) ~= "string" or type(t.target) ~= "table" then
			return settled(nil, "ScheduleSave: a task needs its title, prompt, rrule and target")
		end
		local saved = table.clone(t)
		saved.updatedAt = TASK_NOW
		saved.nextAt = if saved.active ~= false then TASK_NOW + 3600 * 1000 else nil
		if type(saved.id) == "string" and saved.id ~= "" then
			for i, x in task_store do
				if x.id == saved.id then
					task_store[i] = saved
					return settled(saved)
				end
			end
			task_store[#task_store + 1] = saved
			return settled(saved)
		end
		saved.id = `task_{#task_store + 1}`
		task_store[#task_store + 1] = saved
		return settled(saved)
	end,
	["ScheduleDelete"] = function(arg: any)
		local id = if type(arg) == "table" then arg.id else nil
		schedule_calls:push({ op = "delete", id = id })
		local over = schedule_over(schedule_delete:peek())
		if over ~= nil then
			return over
		end
		local at = find(task_store, id)
		if at then
			table.remove(task_store, at)
			return settled({ deleted = true })
		end
		return settled({ deleted = false })
	end,
	["ScheduleRunNow"] = function(arg: any)
		local id = if type(arg) == "table" then arg.id else nil
		schedule_calls:push({ op = "run", id = id })
		local over = schedule_over(schedule_run:peek())
		if over ~= nil then
			return over
		end
		local at = if type(id) == "string" then find(task_store, id) else nil
		if not at then
			return settled(nil, `ScheduleRunNow: no such schedule: {id}`)
		end
		local t = task_store[at]
		t.lastRunAt = TASK_NOW
		if type(t.target) == "table" and (t.target.kind == "new_once" or t.target.kind == "new") then
			t.target.session = `chat_{id}_run`
		end
		return settled({ fired = true, session = if type(t.target) == "table" then t.target.session else nil })
	end,
	["DshChatgptAccounts"] = function(arg: any)
		return settled({ provider = if unscoped_chatgpt:peek() then nil else arg.provider, accounts = (if arg.provider == "openai-codex" then codex_accounts:peek() else chatgpt_accounts:peek()) or {} })
	end,
	["DshChatgptSelect"] = function(arg: any)
		local next = {}
		local store = if arg.provider == "openai-codex" then codex_accounts else chatgpt_accounts
		for _, a in store:peek() or {} do
			local copy = table.clone(a)
			copy.active = a.id == arg.accountId
			table.insert(next, copy)
		end
		store:set(next)
		return settled({})
	end,
	["DshChatgptUseReset"] = function(_arg: any)
		return settled({ outcome = "nothing_to_reset" })
	end,
	["DshChatgptStart"] = function()
		if refuse:peek() then
			return failed("refused")
		end
		chatgpt.running = true
		return settled({ url = "https://auth.openai.com/authorize?probe=1", instructions = "" })
	end,
	["DshChatgptStatus"] = function(arg: any)
		local fixture = if arg.provider == "openai-codex" then codex_fixture else chatgpt_fixture
		local status = table.clone(fixture:peek() or { authorized = false, running = chatgpt.running, url = nil })
		status.provider = if unscoped_chatgpt:peek() then nil else arg.provider
		return settled(status)
	end,
	["DshChatgptCancel"] = function()
		chatgpt.running = false
		return settled(nil)
	end,
	["account.me"] = function()
		return settled(table.clone(me))
	end,
	["dictation.call"] = function(arg: any)
		return settled(if arg.op == "status" then { installed = false, size = 670479942 } elseif arg.op == "inputs" then {} else nil)
	end,
	["account.rename"] = function(name: string)
		local r = change(nil)
		if r.error then
			return r
		end
		local first, rest = string.match(name, "^(%S+)%s*(.*)$")
		me.name, me.first_name, me.last_name = name, first or name, rest or ""
		return settled(table.clone(me))
	end,
	-- Profile's title, photo and deleting the account, as the edge answers
	-- (src/settings.rs). `settings_stub_delete_word`: what the next delete
	-- answers ("ok", or a refusal like "last_admin");
	-- `settings_stub_photo_word`: the next photo upload's.
	["account.set_title"] = function(title: string)
		local r = change(nil)
		if r.error then
			return r
		end
		me.title = if title ~= "" then title else nil
		return settled(table.clone(me))
	end,
	["account.photo_size"] = function()
		return settled({ w = 1200, h = 800 })
	end,
	["account.set_photo"] = function(arg: any)
		local r = change(nil)
		if r.error then
			return r
		end
		if photo_word:peek() ~= "ok" then
			local word = photo_word:peek()
			photo_word:set("ok")
			return settled(nil, word)
		end
		photo_n += 1
		me.avatar = string.format("https://staging.zeron.sh/auth/avatars/%032x", photo_n)
		last_crop:set({ x = arg.x, y = arg.y, side = arg.side })
		return settled(table.clone(me))
	end,
	-- A photo's file: the app's own PNG, whatever the address.
	["account.photo_file"] = function()
		return settled("assets/favicons/zeron.png")
	end,
	["account.remove_photo"] = function()
		me.avatar = nil
		return settled(table.clone(me))
	end,
	["account.delete"] = function(arg: any)
		local word = delete_word:peek()
		if arg.confirm ~= me.email then
			return settled(nil, "invalid")
		elseif word ~= "ok" then
			return settled(nil, word)
		end
		return settled(true)
	end,
	["account.delete_signed_in"] = function(arg: any)
		local word = step_up_word:peek()
		if arg.confirm ~= me.email then
			return settled(nil, "invalid")
		elseif word == "wait" then
			return waiting()
		elseif word ~= "ok" then
			return settled(nil, word)
		end
		return settled(true)
	end,
	["account.signing_keys"] = function()
		if keys_mode:peek() == "none" then
			return settled({ keys = {}, new_keys = {} })
		end
		local out = {}
		for _, k in signing_keys do
			table.insert(out, table.clone(k))
		end
		return settled({ keys = out, new_keys = { "k_dev" } })
	end,
	["account.revoke_key"] = function(key: string)
		for _, k in signing_keys do
			if k.key == key then
				k.revoked_at = 1793700000
			end
		end
		return settled(true)
	end,
	["account.members"] = function()
		local out = {}
		for _, m in members do
			table.insert(out, table.clone(m))
		end
		return settled(out)
	end,
	["account.invitations"] = function()
		if role:peek() ~= "admin" then
			return settled(nil, "refused")
		end
		local out = {}
		for _, i in invitations do
			table.insert(out, table.clone(i))
		end
		return settled(out)
	end,
	["account.remove_member"] = function(arg: any)
		local r = change(nil)
		if not r.error then
			local at = find(members, arg.id)
			if at then
				table.remove(members, at)
			end
		end
		return r
	end,
	["account.set_role"] = function(arg: any)
		local r = change(nil)
		if not r.error then
			local at = find(members, arg.id)
			if at then
				members[at].role = arg.role
			end
		end
		return r
	end,
	["account.revoke_invite"] = function(arg: any)
		local r = change(nil)
		if not r.error then
			local at = find(invitations, arg.id)
			if at then
				table.remove(invitations, at)
			end
		end
		return r
	end,
	["account.resend_invite"] = function()
		return change(nil)
	end,
	["account.rename_org"] = function()
		return change(nil)
	end,
	["settings.engine"] = function(arg: any)
		local m = arg.method
		if string.sub(m, 1, 3) == "Set" then
			local r = change(nil)
			if r.error then
				return r
			end
			local get = "Get" .. string.sub(m, 4)
			if m == "SetCommitCredit" then
				engine[get] = { enabled = arg.params.enabled, cloud = false }
			elseif m == "SetWorktreeSettings" then
				local w = table.clone(engine[get])
				w.settings = arg.params
				engine[get] = w
			elseif m == "SetTitleSettings" then
				engine[get] = arg.params
			end
			return settled(engine[get])
		end
		return settled(engine[m])
	end,
	["notifications.get"] = function()
		if refuse:peek() then
			refuse:set(false)
			return settled(nil, "failed")
		end
		return settled(table.clone(banners))
	end,
	["notifications.set"] = function(arg: any)
		local r = change(nil)
		if not r.error then
			for k, v in arg do
				banners[k] = v
			end
		end
		return r
	end,
	["agents.update"] = function()
		return settled(nil)
	end,
	["agents.set_update_policy"] = function()
		return settled(nil)
	end,
	-- The release feed and the engine's updater (src/about.rs): what the
	-- next check and update answer is `settings_stub_about_next`
	-- ("ok", "pending", or an outcome word).
	["about.check"] = function()
		local next = about_next:peek()
		if next == "pending" then
			-- (Still on its way: never settles.)
			return { done = false, after = function() end }
		elseif next ~= "ok" then
			return settled(nil, next)
		end
		return settled({ latest = "0.2.140", available = true })
	end,
	["about.apply"] = function()
		local next = about_next:peek()
		if next == "pending" then
			-- (Still on its way: never settles.)
			return { done = false, after = function() end }
		elseif next ~= "ok" then
			return settled(nil, next)
		end
		return settled({ version = "0.2.140" })
	end,
	-- This computer's fonts (src/fonts.rs), a few of each kind.
	["fonts.list"] = function()
		return settled({
			{ family = "Fira Code", mono = true },
			{ family = "Inter", mono = false },
			{ family = "JetBrains Mono", mono = true },
			{ family = "Noto Serif", mono = false },
		})
	end,
	-- A background image kept: the picked file itself (the app's own PNG,
	-- so a headless host can draw it), 512 square.
	["wallpaper.keep"] = function(arg: any)
		local name = string.match(arg.path, "[^/]+$") or arg.path
		return settled({ path = arg.path, name = name, w = 512, h = 512 })
	end,
}

local function detail(arg: any): string
	if type(arg) == "string" then
		return arg
	elseif type(arg) == "table" then
		return arg.id or arg.method or arg.name or arg.path or arg.url or ""
	end
	return ""
end

local stub = host :: any
stub.call = function(name: string, arg: any?): any
	log(if detail(arg) ~= "" then `{name}:{detail(arg)}` else name)
	local answer = answers[name]
	return if answer then answer(arg) else settled(nil)
end
stub.invoke = function(name: string, arg: any?)
	log(if detail(arg) ~= "" then `{name}:{detail(arg)}` else name)
	if name == "account.cancel_step_up" then
		signed_in_again("cancelled")
	end
end
-- A photo picked: the app's own PNG, so a headless host can draw it.
stub.open_files = function(_opts: any): any
	log("open_files")
	return settled({ { path = "assets/favicons/zeron.png", name = "zeron.png", size = 1, type = "image/png" } })
end

-- The account's own calls (account/api), the same way.
local apis = api :: any
-- (No username yet: a card's art is WebP, which a headless host can't
-- draw, so Profile shows the way to one.)
apis.username = function(): any
	log("account.username")
	return settled(nil)
end
apis.check = function(name: string): any
	log(`account.check_username:{name}`)
	return settled({ available = true })
end
apis.card = function(name: string): any
	log(`account.card:{name}`)
	return settled(table.clone(card))
end
apis.orgs = function(): any
	log("account.orgs")
	return settled({ { id = "om_ada", organization_id = "org_1", name = "Analytical Engines", role = role:peek() } })
end
apis.invite = function(_org: string, emails: { string }): any
	log(`account.invite:{#emails}`)
	if refuse:peek() then
		refuse:set(false)
		return settled(nil, "refused")
	end
	local sent, failed = {}, {}
	for _, e in emails do
		if string.find(e, "taken", 1, true) then
			table.insert(failed, { email = e, error = "exists" })
		else
			table.insert(sent, e)
			table.insert(invitations, 1, { id = `inv_{e}`, email = e, state = "pending", created_at = "2026-10-31T10:00:00Z" })
		end
	end
	return settled({ sent = sent, failed = failed })
end
-- This computer's agents and another device's (account/agents): the one
-- runtime this build runs, on each.
local agents = require("@account/agents")
local function agents_of(target: string?): any
	-- Cloud (the engine answers for it from the vault): the DeepSeek
	-- Harness, always installed.
	if target == "cloud" then
		local function one(id: string, name: string, mark: string, accounts: { any }): any
			return { id = id, name = name, mark = mark, installed = true, can_install = false, enabled = #accounts > 0, version = nil, installing = false, login = "idle", accounts = accounts }
		end
		return {
			state = "ready",
			target = target,
			list = {
				one("deepseek", "DeepSeek", "provider-deepseek", { { id = "c1", email = "ada@example.com", plan = "Pro", session = 0.1, week = 0.2, active = true } }),
			},
		}
	end
	return {
		state = "ready",
		target = target,
		list = {
			{
				id = "deepseek",
				name = "DeepSeek",
				mark = "provider-deepseek",
				installed = true,
				can_install = true,
				enabled = true,
				version = "1.0.0",
				installing = false,
				login = "idle",
				-- One signed-in account, so the page's accounts section has a
				-- row to show (its plan, its two windows).
				accounts = {
					{
						id = "a1",
						email = "d@example.com",
						plan = "ChatGPT Plus",
						session = 0.01,
						week = 0.22,
						active = true,
						banked = 1,
						resets = { { id = "r1", title = "Reset" } },
					},
				},
			},
		},
	}
end
local agent_stub = agents :: any
agent_stub.load = function()
	log("agents.load")
	agents.now:set(agents_of(agents.now:peek().target))
end
agent_stub.target = function(device: string?)
	log(`agents.target:{device or ""}`)
	agents.now:set(agents_of(device))
end
-- A sign-in here is Claude's that asks for a code pasted back (no loopback
-- port free); the code finishes it with an account, or Cancel gives it up.
local function set_login(id: string, login: string, account: any?)
	local now = table.clone(agents.now:peek())
	local list = {}
	for _, a in now.list do
		local one = table.clone(a)
		if one.id == id then
			one.login = login
			if account then
				one.accounts = { account }
			end
		end
		table.insert(list, one)
	end
	now.list = list
	agents.now:set(now)
end
agent_stub.sign_in = function(id: string)
	log(`agents.sign_in:{id}`)
	set_login(id, "code")
end
agent_stub.cancel = function(id: string)
	log(`agents.cancel:{id}`)
	set_login(id, "idle")
end
agent_stub.complete = function(id: string, code: string)
	log(`agents.complete:{id}:{code}`)
	set_login(id, "idle", { id = "acc_1", email = "ada@example.net", plan = "Max", session = 0.2, week = 0.4, active = true })
end

-- Renaming a device (account/sessions), as the engine would answer.
local sessions_stub = require("@account/sessions") :: any
sessions_stub.rename_device = function(id: string, name: string): any
	log(`sessions.rename_device:{id}:{name}`)
	return change(nil)
end

local auths = auth :: any
auths.sign_out = function(done: (() -> ())?)
	log("account.sign_out")
	auth.account:set({ state = "signed_out", env = "staging" } :: any)
	if done then
		done()
	end
end

-- The launch before the engine has answered, and the engine saying signed out.
ui.command {
	id = "settings_stub.restoring",
	title = "The engine hasn't answered yet",
	run = function()
		auth.account:set({ state = "restoring", env = "staging" } :: any)
	end,
}
ui.command {
	id = "settings_stub.signed_out",
	title = "The engine says signed out",
	run = function()
		auth.account:set({ state = "signed_out", env = "staging" } :: any)
	end,
}

ui.command {
	id = "settings_stub.member_joins",
	title = "A member joins",
	run = function()
		table.insert(members, { id = "om_edsger", name = "Edsger Dijkstra", email = "edsger@example.net", role = "member" })
		local at = find(invitations, "inv_1")
		if at then
			table.remove(invitations, at)
		end
		-- (The page asks again: the edge doesn't tell.)
		local store = require("@app/screens/settings/store")
		store.load_members()
		store.load_invitations()
	end,
}
ui.command {
	id = "settings_stub.signed_in_again",
	title = "Signed in again in the browser",
	run = function()
		signed_in_again(nil)
	end,
}
ui.command {
	id = "settings_stub.sign_in",
	title = "Signed in",
	run = function()
		auth.account:set({
			state = "signed_in",
			env = "staging",
			user = { id = "user_ada", email = "ada@example.net", first_name = "Ada" },
			organization_id = "org_1",
		} :: any)
	end,
}
ui.command {
	id = "settings_stub.open_account",
	title = "Open Settings' old Account page",
	run = function()
		require("@app/screens/shell/route").settings("account")
	end,
}
-- What the main window's theme overrides now (ds.look): its accent, a
-- syntax colour and its fonts, for a test to read.
local window_theme = ui.state("settings_stub_window_theme", {} :: { [string]: any })
ui.command {
	id = "settings_stub.read_theme",
	title = "Read the window's theme",
	run = function()
		local t = require("@ds/look").window_theme() or {}
		window_theme:set({
			accent = t.accent,
			keyword = t["zeron.syntax.keyword"],
			body = t["font.size.md"],
			window = t["zeron.window"],
			sans = t["font.sans"],
			mono = t["font.mono"],
			secondary = t["text.secondary"],
			tertiary = t["text.tertiary"],
			-- "Accent colour throughout": a status dot, a find's highlight,
			-- an @file token, a list marker, and a link's ink (nil: the
			-- text's own).
			working_dot = t["zeron.status.dot.working"],
			working = t["zeron.status.working"],
			find = t["zeron.editor.match"],
			file_token = t["zeron.composer.token.file"],
			marker = t["zeron.prose.marker"],
			link = require("@ds/look").accented_ink(nil)(),
			-- What the accent's own token is, and what the theme's says.
			accented_ink = ui.theme.get("zeron.accented.ink"),
			accent_ink = ui.theme.get("accent"),
		})
	end,
}
ui.command {
	id = "settings_stub.refuse_next",
	title = "Refuse the next change",
	run = function()
		refuse:set(true)
	end,
}
ui.command {
	id = "settings_stub.not_admin",
	title = "Not an admin",
	run = function()
		role:set("member")
	end,
}

-- Cloud, Billing and GitHub for Cloud (Settings → Cloud and → Billing),
-- the same way: none until `cloud_stub.offer`.
require("./cloud_stub")

return M
