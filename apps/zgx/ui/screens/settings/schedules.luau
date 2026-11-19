-- Settings → Automation tasks: the engine's scheduled tasks
-- (crates/engine/src/schedules.rs), each a prompt run on its rule — in
-- the chat open as the task was made, or a new one. `Schedules` lists
-- them, `ScheduleSave` writes (`{task}`: an empty `id` creating, else
-- the task it names replaced whole), `ScheduleDelete` and
-- `ScheduleRunNow` take `{id}` (run now fires it once, its rule and
-- pause as they were).
-- A task's rule is the two lines an iCalendar one is — `DTSTART…` then
-- `RRULE:…` — written here for once, daily, weekdays and weekly, in UTC
-- or a fixed offset (the zones the engine takes: a named one is
-- refused, not guessed), and read back for the row's "Every weekday
-- at 09:00".

local host = require("@app/screens/settings/host")
local ds = require("@ds")
local search = require("@app/screens/settings/search")
local route = require("@app/screens/shell/route")
local locale = require("@ds/locale")

local M = { ID = "schedules", TITLE = "Automation tasks" }
search.index(M.ID, ui.t("settings.schedules.title"), ui.t("settings.schedules.terms"), {
	{ key = "tasks", title = ui.t("settings.schedules.tasks"), terms = ui.t("settings.schedules.tasks_terms") },
	{ key = "new", title = ui.t("settings.schedules.new"), terms = ui.t("settings.schedules.new_terms") },
})

local function say(err: any): string
	return if type(err) == "table" and type(err.message) == "string" then err.message else tostring(err)
end

-- os.date's wday (Sunday 1) to its BYDAY code, and back.
local WDAY_CODE = { "SU", "MO", "TU", "WE", "TH", "FR", "SA" }
local CODE_WDAY = { SU = 1, MO = 2, TU = 3, WE = 4, TH = 5, FR = 6, SA = 7 }
local WEEKDAYS = { MO = true, TU = true, WE = true, TH = true, FR = true }
-- DTSTART;TZID= accepts these for UTC (the engine's aliases), or a fixed
-- `+05:30` — a named zone is refused.
local UTC_ZONES = { UTC = true, GMT = true, Z = true, ["Etc/UTC"] = true, ["Etc/GMT"] = true, ["Etc/Universal"] = true }

type Rule = {
	zone: string, -- "UTC" or the offset in words ("UTC+05:30")
	offset: number, -- seconds east of UTC
	date: string, -- DTSTART's yyyymmdd
	time: string, -- DTSTART's hhmmss
	freq: string,
	interval: number,
	days: { string }?,
	hours: { number }?,
	minutes: { number }?,
	count: number?,
}

type Fields = {
	freq: string, -- once | daily | weekdays | weekly | custom
	date: string, -- once's, "YYYY-MM-DD"
	time: string, -- "HH:MM"
	day: string, -- weekly's BYDAY code
	zone: string, -- utc | device | fixed
	offset: number, -- fixed's, seconds east of UTC
	rrule: string?, -- custom's, kept verbatim
}

-- A date's weekday (os.date's wday): noon, where every zone agrees on
-- the date.
local function wday_of(y: number, m: number, d: number): number
	return ((os.date("*t", os.time({ year = y, month = m, day = d, hour = 12, min = 0, sec = 0 }) :: number)) :: any).wday :: number
end

-- This machine's offset east of UTC, seconds: the same instant read as
-- UTC fields differs by exactly that.
local function device_offset(): number
	local now = os.time()
	return now - os.time((os.date("!*t", now) :: any))
end

-- `UTC+05:30` (or `UTC`) for an offset in seconds.
local function offset_words(seconds: number): string
	if seconds == 0 then
		return "UTC"
	end
	local a = math.abs(seconds)
	return `UTC{if seconds > 0 then "+" else "-"}{string.format("%02d:%02d", a // 3600, a % 3600 // 60)}`
end

-- `+05:30` (a TZID's fixed-offset form) for an offset in seconds.
local function offset_code(seconds: number): string
	local a = math.abs(seconds)
	return `{if seconds >= 0 then "+" else "-"}{string.format("%02d:%02d", a // 3600, a % 3600 // 60)}`
end

-- A task's two-line rule as a Rule, nil when it isn't one this page
-- reads (another tool's: kept verbatim on an edit, shown "Custom").
local function parse(src: any): Rule?
	if type(src) ~= "string" then
		return nil
	end
	local lines = {}
	for line in string.gmatch(src, "[^\r\n]+") do
		table.insert(lines, line)
	end
	if #lines ~= 2 then
		return nil
	end
	local head, value = string.match(lines[1], "^DTSTART(.*):(.*)$")
	if head == nil or value == nil then
		return nil
	end
	local zulu = string.sub(value, -1) == "Z"
	if zulu then
		value = string.sub(value, 1, -2)
	end
	local date, at = string.match(value, "^(%d%d%d%d%d%d%d%d)T(%d%d%d%d%d%d)$")
	if date == nil then
		return nil
	end
	local zone, offset = "UTC", 0
	if head ~= "" then
		local tzid = string.match(head, "^;TZID=(.*)$")
		if tzid == nil then
			return nil
		end
		if not UTC_ZONES[tzid] then
			local sign, h, m = string.match(tzid, "^([+-])(%d%d?):?(%d?%d?)$")
			local hh, mm = tonumber(h), tonumber(m)
			if sign == nil or hh == nil or hh >= 24 or (mm or 0) >= 60 then
				return nil
			end
			offset = (if sign == "+" then 1 else -1) * (hh * 3600 + (mm or 0) * 60)
			zone = offset_words(offset)
		end
	end
	if zulu and offset ~= 0 then
		return nil -- 'Z' with a non-UTC zone, refused as the engine does.
	end
	local body = string.match(lines[2], "^RRULE:(.*)$")
	if body == nil then
		return nil
	end
	local rule: Rule = { zone = zone, offset = offset, date = date, time = at, freq = "", interval = 1 }
	for part in string.gmatch(body, "[^;]+") do
		local k, v = string.match(part, "^(%w+)=(.*)$")
		if k == "FREQ" then
			rule.freq = v
		elseif k == "INTERVAL" then
			rule.interval = tonumber(v) or 0
			if rule.interval < 1 then
				return nil
			end
		elseif k == "BYDAY" then
			local days = {}
			for d in string.gmatch(v, "[^,]+") do
				if CODE_WDAY[d] == nil then
					return nil
				end
				table.insert(days, d)
			end
			rule.days = days
		elseif k == "BYHOUR" or k == "BYMINUTE" then
			local limit = if k == "BYHOUR" then 24 else 60
			local list = {}
			for n in string.gmatch(v, "[^,]+") do
				local x = tonumber(n)
				if x == nil or x < 0 or x >= limit or x % 1 ~= 0 then
					return nil
				end
				table.insert(list, x)
			end
			if k == "BYHOUR" then
				rule.hours = list
			else
				rule.minutes = list
			end
		elseif k == "COUNT" then
			rule.count = tonumber(v)
			if rule.count == nil or rule.count < 1 or rule.count % 1 ~= 0 then
				return nil
			end
		elseif k ~= "UNTIL" then
			return nil -- a part this page doesn't write: not its own rule.
		end
	end
	if rule.freq == "" then
		return nil
	end
	return rule
end

local function is_weekdays(days: { string }?): boolean
	if days == nil or #days ~= 5 then
		return false
	end
	for _, d in days do
		if not WEEKDAYS[d] then
			return false
		end
	end
	return true
end

local function day_name(code: string): string
	return ui.t("time.weekday_short", { d = tostring(CODE_WDAY[code]) }):get()
end

local function day_names(days: { string }): string
	local out = {}
	for _, d in days do
		table.insert(out, day_name(d))
	end
	return table.concat(out, ", ")
end

-- A rule's run time: BYHOUR/BYMINUTE's first, else DTSTART's.
local function clock_of(rule: Rule): (number, number)
	local h = if rule.hours and rule.hours[1] then rule.hours[1] else tonumber(string.sub(rule.time, 1, 2)) or 0
	local m = if rule.minutes and rule.minutes[1] then rule.minutes[1] else tonumber(string.sub(rule.time, 3, 4)) or 0
	return h, m
end

-- A rule in words, for a row: "Every weekday at 09:00 UTC". One this
-- page didn't write reads "Custom schedule".
local function frequency(rule: Rule?): string
	local function t(key: string, args: any): string
		return ui.t(`settings.schedules.freq.{key}`, args):get()
	end
	if rule == nil then
		return t("custom", nil)
	end
	local h, m = clock_of(rule)
	local at = string.format("%02d:%02d", h, m)
	local zone = rule.zone
	if rule.count == 1 then
		local y = tonumber(string.sub(rule.date, 1, 4)) :: number
		local mo = tonumber(string.sub(rule.date, 5, 6)) :: number
		local d = tonumber(string.sub(rule.date, 7, 8)) :: number
		local date = ui.t("time.date", { m = tostring(mo), day = tostring(d), year = tostring(y) }):get()
		return t("once", { date = date, time = at, zone = zone })
	end
	if rule.freq == "MINUTELY" then
		return t("minutely", { n = rule.interval })
	end
	if rule.freq == "HOURLY" then
		return t("hourly", { n = rule.interval })
	end
	if is_weekdays(rule.days) then
		return t("weekdays", { time = at, zone = zone })
	end
	if rule.freq == "DAILY" and rule.days == nil then
		if rule.interval == 1 then
			return t("daily", { time = at, zone = zone })
		end
		return t("every_days", { n = rule.interval, time = at, zone = zone })
	end
	if rule.freq == "WEEKLY" or rule.freq == "DAILY" then
		local days = rule.days
		if days == nil or #days == 0 then
			local y = tonumber(string.sub(rule.date, 1, 4)) :: number
			local mo = tonumber(string.sub(rule.date, 5, 6)) :: number
			local d = tonumber(string.sub(rule.date, 7, 8)) :: number
			days = { WDAY_CODE[wday_of(y, mo, d)] }
		end
		if #days == 1 and rule.interval == 1 and rule.freq == "WEEKLY" then
			return t("weekly", { day = day_name(days[1]), time = at, zone = zone })
		end
		return t("weekly_days", { days = day_names(days), time = at, zone = zone })
	end
	return t("custom", nil)
end

-- A real calendar date (os.time moves an impossible one into the next
-- month: read back and compare).
local function real_date(y: number, m: number, d: number): boolean
	local t = os.time({ year = y, month = m, day = d, hour = 12, min = 0, sec = 0 })
	local f = (os.date("*t", t) :: any)
	return f.year == y and f.month == m and f.day == d
end

-- The editor's fields as the two-line rule the engine parses, or nil
-- and the field at fault. `zone` "fixed" writes its kept offset;
-- "device" writes this machine's now (passed in, so the field's label
-- and the rule agree).
local function build_rrule(f: Fields, device: number): (string?, string?)
	if f.freq == "custom" then
		return f.rrule, nil
	end
	local h, m = string.match(f.time, "^(%d%d?):(%d%d)$")
	local hh, mm = tonumber(h), tonumber(m)
	if hh == nil or mm == nil or hh >= 24 or mm >= 60 then
		return nil, "time"
	end
	local offset = if f.zone == "utc" then 0 elseif f.zone == "fixed" then f.offset else device
	local date
	if f.freq == "once" then
		local y, mo, d = string.match(f.date, "^(%d%d%d%d)%-(%d%d)%-(%d%d)$")
		if y == nil or not real_date(tonumber(y) :: number, tonumber(mo) :: number, tonumber(d) :: number) then
			return nil, "date"
		end
		date = `{y}{mo}{d}`
	else
		-- Today's date in the zone (the instant that offset ahead, read
		-- as UTC fields): the rule's first candidate run.
		date = (os.date("!%Y%m%d", os.time() + offset) :: string)
	end
	local at = string.format("%02d%02d00", hh, mm)
	local dtstart = if f.zone == "utc" then `DTSTART:{date}T{at}Z` else `DTSTART;TZID={offset_code(offset)}:{date}T{at}`
	local clock = `BYHOUR={hh};BYMINUTE={mm}`
	local body
	if f.freq == "once" then
		-- One run, at DTSTART.
		body = "FREQ=DAILY;COUNT=1"
	elseif f.freq == "daily" then
		body = `FREQ=DAILY;{clock}`
	elseif f.freq == "weekdays" then
		body = `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;{clock}`
	elseif f.freq == "weekly" then
		body = `FREQ=WEEKLY;BYDAY={f.day};{clock}`
	else
		return nil, "time"
	end
	return `{dtstart}\nRRULE:{body}`, nil
end

-- Where a save runs the prompt, `run_in` the editor's pick. "This chat"
-- is the chat open now — a session-targeted task edited while no chat
-- is open keeps the one it names. A `new`/`new_once` target keeps the
-- chat the engine recorded for it (`session`) and the folder it works
-- in (`cwd`, else the open chat's or project's).
local function target_for(run_in: string, existing: any): ({ [string]: any }?, string?)
	if run_in == "session" then
		local open = route.session:peek()
		local id = if open then open.id
			elseif existing and type(existing.target) == "table" then existing.target.session
			else nil
		if type(id) ~= "string" then
			return nil, "run_in"
		end
		return { kind = "session", session = id }, nil
	end
	local cwd, kept = "", nil
	if existing and type(existing.target) == "table" then
		if type(existing.target.cwd) == "string" then
			cwd = existing.target.cwd
		end
		if type(existing.target.session) == "string" then
			kept = existing.target.session
		end
	end
	if cwd == "" then
		local open = route.session:peek()
		local project = route.project:peek()
		cwd = if open and type(open.cwd) == "string" and open.cwd ~= ""
			then open.cwd
			elseif project and type(project.path) == "string" then project.path
			else ""
	end
	local target: { [string]: any } = { kind = run_in, cwd = cwd }
	if kept ~= nil then
		target.session = kept
	end
	return target, nil
end

-- A row in the list: the task's name, its rule in words and next run,
-- Active or Paused said, and its actions (pause or resume, run now,
-- edit, delete — delete asks first).
local Row = ui.component(function(props: {
	now: State<any>,
	edit: (task: any) -> (),
	reload: () -> (),
}): Node
	local now = props.now
	local busy = ui.use_state(false)
	local note = ui.use_state(nil :: string?)
	local problem = ui.use_state(nil :: string?)
	local removing = ui.use_state(false)
	local remove_busy = ui.use_state(false)

	local function task(): any
		return now:get()
	end

	local function set_active(active: boolean)
		if busy:peek() then
			return
		end
		busy:set(true)
		note:set(nil)
		problem:set(nil)
		-- The whole task back with `active` flipped: its rule, target and
		-- the chat a new_once made all stay.
		local next = table.clone(task())
		next.active = active
		host.call("ScheduleSave", { task = next }):after(function(answer)
			busy:set(false)
			if type(answer) == "table" and type(answer.id) == "string" then
				props.reload()
			else
				problem:set(ui.t("settings.schedules.pause_failed"):get())
			end
		end, function(err)
			busy:set(false)
			problem:set(say(err))
		end)
	end

	local function run_now()
		if busy:peek() then
			return
		end
		busy:set(true)
		note:set(nil)
		problem:set(nil)
		host.call("ScheduleRunNow", { id = task().id }):after(function(answer)
			busy:set(false)
			if type(answer) == "table" and answer.fired == true then
				note:set(ui.t("settings.schedules.ran"):get())
				props.reload()
			else
				problem:set(ui.t("settings.schedules.run_failed"):get())
			end
		end, function(err)
			busy:set(false)
			problem:set(say(err))
		end)
	end

	local function remove()
		remove_busy:set(true)
		host.call("ScheduleDelete", { id = task().id }):after(function(answer)
			remove_busy:set(false)
			if type(answer) == "table" and answer.deleted == true then
				removing:set(false)
				props.reload()
			else
				problem:set(ui.t("settings.schedules.delete_failed"):get())
			end
		end, function(err)
			remove_busy:set(false)
			problem:set(say(err))
		end)
	end

	return ds.Settings.Row {
		label = function(): string
			local t = task()
			return if type(t.title) == "string" then t.title else ""
		end,
		description = function(): string
			local t = task()
			local at = frequency(parse(t.rrule))
			if type(t.nextAt) == "number" then
				return `{at} · {ui.t("settings.schedules.next_run", { when = locale.exact(t.nextAt / 1000) }):get()}`
			end
			return at
		end,
		detail = function(): string?
			return problem:get() or note:get()
		end,
		truncate = true,
		control = ui.row {
			items = "center",
			gap = "$space.3",
			ui.text {
				text_size = "$font.size.sm",
				color = "$text.secondary",
				function(): string
					return (if task().active ~= false then ui.t("settings.schedules.active") else ui.t("settings.schedules.paused")):get()
				end,
			},
			ds.Button {
				tone = "ghost",
				size = "sm",
				disabled = busy,
				on_click = function()
					set_active(not (task().active ~= false))
				end,
				function(): string
					return (if task().active ~= false then ui.t("settings.schedules.pause") else ui.t("settings.schedules.resume")):get()
				end,
			},
			ds.Button {
				tone = "ghost",
				size = "sm",
				disabled = busy,
				on_click = run_now,
				ui.t("settings.schedules.run_now"),
			},
			ds.IconButton {
				icon = "pen",
				size = "sm",
				label = function(): string
					return ui.t("settings.schedules.edit_named", { name = task().title or "" }):get()
				end,
				on_click = function()
					props.edit(task())
				end,
			},
			ds.IconButton {
				icon = "trash-bin-minimalistic",
				size = "sm",
				tone = "danger",
				label = function(): string
					return ui.t("settings.schedules.delete_named", { name = task().title or "" }):get()
				end,
				on_click = function()
					removing:set(true)
				end,
			},
			-- (The modal stands over the page wherever it is built.)
			ds.Dialog {
				open = removing,
				title = function(): string
					return ui.t("settings.schedules.delete_title", { name = task().title or "" }):get()
				end,
				body = ui.t("settings.schedules.delete_body"),
				tone = "danger",
				size = "sm",
				confirm = {
					label = ui.t("settings.schedules.delete_confirm"),
					busy = function(): boolean
						return remove_busy:get()
					end,
					on_click = remove,
				},
			},
		},
	}
end)

function M.view(): Node
	-- Asked as the page opens; Try again asks again.
	local res = ui.resource(function(): any
		return host.call("Schedules", {}):await()
	end)
	local tasks = ui.computed(function(): { any }
		local d = res.data:get()
		return if type(d) == "table" and type(d.tasks) == "table" then d.tasks else {}
	end)
	local function kind(): string
		if res.data:get() ~= nil then
			return if #tasks:get() > 0 then "list" else "empty"
		end
		return if res.status:get() == "failed" then "failed" else "loading"
	end

	-- The editor, over the page's right edge: `editing` the task it
	-- works on (nil, a new one), its fields filled as it opens so a
	-- failed save loses nothing.
	local editing = ui.use_state(nil :: any)
	local editor_open = ui.use_state(false)
	local name = ui.use_state("")
	local instruction = ui.use_state("")
	local run_in = ui.use_state("new_once")
	local freq = ui.use_state("daily")
	local day = ui.use_state("MO")
	local date = ui.use_state("")
	local at = ui.use_state("09:00")
	local zone = ui.use_state("device")
	local kept_rrule = ui.use_state(nil :: string?)
	local kept_offset = ui.use_state(0)
	local busy = ui.use_state(false)
	local problem = ui.use_state(nil :: string?)
	local bad = ui.use_state(nil :: string?)

	local function open_editor(task: any?)
		local rule = if task then parse(task.rrule) else nil
		editing:set(task)
		name:set(if task and type(task.title) == "string" then task.title else "")
		instruction:set(if task and type(task.prompt) == "string" then task.prompt else "")
		local kind = if task and type(task.target) == "table" then task.target.kind else nil
		if kind ~= "session" and kind ~= "new_once" and kind ~= "new" then
			kind = if route.session:peek() then "session" else "new_once"
		end
		run_in:set(kind)
		local f = "daily"
		if task then
			if rule == nil then
				f = "custom"
			elseif rule.count == 1 then
				f = "once"
			elseif rule.freq == "DAILY" and rule.interval == 1 and rule.days == nil then
				f = "daily"
			elseif (rule.freq == "DAILY" or rule.freq == "WEEKLY") and rule.interval == 1 and is_weekdays(rule.days) then
				f = "weekdays"
			elseif rule.freq == "WEEKLY" and rule.interval == 1 and (rule.days == nil or #rule.days == 1) then
				f = "weekly"
			else
				f = "custom"
			end
		end
		freq:set(f)
		local code = "MO"
		if f == "weekly" and rule then
			if rule.days and rule.days[1] then
				code = rule.days[1]
			else
				local y = tonumber(string.sub(rule.date, 1, 4)) :: number
				local mo = tonumber(string.sub(rule.date, 5, 6)) :: number
				local d = tonumber(string.sub(rule.date, 7, 8)) :: number
				code = WDAY_CODE[wday_of(y, mo, d)]
			end
		end
		day:set(code)
		if rule then
			local y, mo, d = string.sub(rule.date, 1, 4), string.sub(rule.date, 5, 6), string.sub(rule.date, 7, 8)
			date:set(`{y}-{mo}-{d}`)
			local hh, mm = clock_of(rule)
			at:set(string.format("%02d:%02d", hh, mm))
			kept_offset:set(rule.offset)
			zone:set(if rule.offset == 0 then "utc" elseif rule.offset == device_offset() then "device" else "fixed")
		else
			date:set((os.date("!%Y-%m-%d", os.time() + device_offset() + 86400) :: string))
			at:set("09:00")
			kept_offset:set(0)
			zone:set("device")
		end
		kept_rrule:set(if task and type(task.rrule) == "string" then task.rrule else nil)
		problem:set(nil)
		bad:set(nil)
		busy:set(false)
		editor_open:set(true)
	end

	local function fault(field: string)
		bad:set(field)
		problem:set(ui.t(`settings.schedules.needed_{field}`):get())
	end

	local function save()
		if busy:peek() then
			return
		end
		local task = editing:peek()
		local title = string.gsub(name:peek(), "^%s*(.-)%s*$", "%1")
		local prompt = string.gsub(instruction:peek(), "^%s*(.-)%s*$", "%1")
		if title == "" then
			fault("name")
			return
		end
		if (utf8.len(title) or #title) > 120 then
			bad:set("name")
			problem:set(ui.t("settings.schedules.too_long"):get())
			return
		end
		if prompt == "" then
			fault("instruction")
			return
		end
		local target, target_bad = target_for(run_in:peek(), task)
		if target == nil then
			fault(target_bad or "run_in")
			return
		end
		local rrule, rrule_bad = build_rrule({
			freq = freq:peek(),
			date = date:peek(),
			time = at:peek(),
			day = day:peek(),
			zone = zone:peek(),
			offset = kept_offset:peek(),
			rrule = kept_rrule:peek(),
		}, device_offset())
		if rrule == nil then
			fault(rrule_bad or "time")
			return
		end
		local next: { [string]: any } = {
			title = title,
			prompt = prompt,
			rrule = rrule,
			target = target,
			active = if task then task.active ~= false else true,
		}
		if task and type(task.id) == "string" then
			next.id = task.id
		end
		busy:set(true)
		problem:set(nil)
		bad:set(nil)
		host.call("ScheduleSave", { task = next }):after(function(answer)
			busy:set(false)
			if type(answer) == "table" and type(answer.id) == "string" then
				editor_open:set(false)
				editing:set(nil)
				res.reload()
			else
				problem:set(ui.t("settings.schedules.save_failed"):get())
			end
		end, function(err)
			-- The engine's own words ("rrule has no occurrence in the
			-- future", "a rrule needs a time"): the sheet stays open on
			-- them, the fields as typed.
			busy:set(false)
			problem:set(say(err))
		end)
	end

	-- The selects' options as functions (a select follows one each read):
	-- their labels are messages, which a state can't hold.
	local function run_in_options(): { any }
		return {
			{ value = "session", label = ui.t("settings.schedules.run_in.session"), disabled = route.session:get() == nil },
			{ value = "new_once", label = ui.t("settings.schedules.run_in.new_once") },
			{ value = "new", label = ui.t("settings.schedules.run_in.new") },
		}
	end
	local function freq_options(): { any }
		local out: { any } = {
			{ value = "once", label = ui.t("settings.schedules.freq_opts.once") },
			{ value = "daily", label = ui.t("settings.schedules.freq_opts.daily") },
			{ value = "weekdays", label = ui.t("settings.schedules.freq_opts.weekdays") },
			{ value = "weekly", label = ui.t("settings.schedules.freq_opts.weekly") },
		}
		if freq:get() == "custom" then
			table.insert(out, { value = "custom", label = ui.t("settings.schedules.freq_opts.custom") })
		end
		return out
	end
	local function day_options(): { any }
		local out = {}
		for _, code in { "MO", "TU", "WE", "TH", "FR", "SA", "SU" } do
			table.insert(out, { value = code, label = day_name(code) })
		end
		return out
	end
	local function zone_options(): { any }
		local out: { any } = {
			{ value = "utc", label = "UTC" },
			{ value = "device", label = ui.t("settings.schedules.zone_device", { offset = offset_words(device_offset()) }) },
		}
		if zone:get() == "fixed" then
			table.insert(out, { value = "fixed", label = offset_words(kept_offset:get()) })
		end
		return out
	end

	local function editor(): Node
		return ds.Sheet {
			open = editor_open,
			title = function(): string
				return (if editing:get() then ui.t("settings.schedules.editor_edit") else ui.t("settings.schedules.editor_new")):get()
			end,
			size = 420,
			foot = ui.row {
				w_full = true,
				items = "center",
				gap = "$space.3",
				ui.column {
					grow = 1,
					min_w = 0,
					ui.show(function(): boolean
						return problem:get() ~= nil
					end, ui.text {
						function(): string
							return problem:get() or ""
						end,
						text_size = "$font.size.sm",
						color = "$danger",
						wrap = true,
					}),
				},
				ds.Button {
					tone = "ghost",
					disabled = busy,
					on_click = function()
						editor_open:set(false)
					end,
					ui.t("common.cancel"),
				},
				ds.Button {
					busy = busy,
					on_click = save,
					ui.t("settings.schedules.save"),
				},
			},
			ds.TextField {
				value = name,
				label = ui.t("settings.schedules.field_name"),
				placeholder = ui.t("settings.schedules.field_name_placeholder"),
				w_full = true,
				error = function(): string?
					return if bad:get() == "name" then problem:get() else nil
				end,
			},
			ds.TextArea {
				value = instruction,
				label = ui.t("settings.schedules.field_instruction"),
				placeholder = ui.t("settings.schedules.field_instruction_placeholder"),
				rows = 4,
				max_rows = 8,
				w_full = true,
				error = function(): string?
					return if bad:get() == "instruction" then problem:get() else nil
				end,
			},
			ds.Select {
				value = run_in,
				label = ui.t("settings.schedules.field_run_in"),
				options = run_in_options,
				w_full = true,
				error = function(): string?
					return if bad:get() == "run_in" then problem:get() else nil
				end,
			},
			ds.Select {
				value = freq,
				label = ui.t("settings.schedules.field_when"),
				options = freq_options,
				w_full = true,
			},
			ui.show(function(): boolean
				return freq:get() == "custom"
			end, ui.text {
				function(): string
					return string.gsub(kept_rrule:get() or "", "\n", " · ")
				end,
				font = "$font.mono",
				text_size = "$font.size.xs",
				color = "$text.tertiary",
				wrap = true,
			}),
			ui.show(function(): boolean
				return freq:get() == "weekly"
			end, ds.Select {
				value = day,
				label = ui.t("settings.schedules.field_day"),
				options = day_options,
				w_full = true,
			}),
			ui.show(function(): boolean
				return freq:get() == "once"
			end, ds.TextField {
				value = date,
				label = ui.t("settings.schedules.field_date"),
				hint = ui.t("settings.schedules.date_hint"),
				w_full = true,
				error = function(): string?
					return if bad:get() == "date" then problem:get() else nil
				end,
			}),
			ui.show(function(): boolean
				return freq:get() ~= "custom"
			end, ui.row {
				gap = "$space.3",
				items = "end",
				ds.TextField {
					value = at,
					label = ui.t("settings.schedules.field_time"),
					hint = ui.t("settings.schedules.time_hint"),
					w = 130,
					error = function(): string?
						return if bad:get() == "time" then problem:get() else nil
					end,
				},
				ds.Select {
					value = zone,
					label = ui.t("settings.schedules.field_zone"),
					options = zone_options,
					w = 210,
				},
			}),
		}
	end

	return ds.Settings.Page {
		title = ui.t("settings.schedules.title"),
		description = ui.t("settings.schedules.description"),
		trail = search.anchor(
			"new",
			ds.Button {
				tone = "secondary",
				size = "sm",
				on_click = function()
					open_editor(nil)
				end,
				ui.t("settings.schedules.new"),
			},
			true
		),
		trail_top = true,
		search.anchor("tasks", ds.Settings.Section {
			ui.presence(kind, function(k: string): Node
				if k == "loading" then
					-- (Said, not only drawn: the bars are hidden from assistive
					-- technology.)
					return ui.column {
						w_full = true,
						label = ui.t("common.loading"),
						live = "polite",
						ds.Settings.Group {
							ds.SkeletonRow { title = 130, line = 200, trail = 90 },
							ds.SkeletonRow { title = 110, line = 180, trail = 90 },
						},
					}
				elseif k == "failed" then
					return ds.EmptyState {
						icon = "calendar",
						size = "sm",
						title = ui.t("settings.schedules.failed_title"),
						body = function(): string
							local err = res.error:get()
							return if err then `{say(err)} {ui.t("settings.schedules.failed_hint"):get()}` else ui.t("settings.schedules.failed_hint"):get()
						end,
						action = { label = ui.t("settings.schedules.retry"), on_click = res.reload },
					}
				elseif k == "empty" then
					return ds.EmptyState {
						icon = "calendar",
						size = "sm",
						title = ui.t("settings.schedules.empty_title"),
						body = ui.t("settings.schedules.empty_body"),
					}
				end
				return ds.Settings.Each(tasks, function(_t: any, _i: number, now: State<any>): Node
					return Row { now = now, edit = open_editor, reload = res.reload }
				end, {
					key = function(t: any): string
						return tostring(t.id)
					end,
					wash = true,
				})
			end, { w_full = true, enter = { opacity = { 0, 1 }, duration = 120 }, exit = { opacity = 0, duration = 60 }, mode = "wait" }),
		}),
		-- The editor and each row's delete ask stand over the page.
		editor(),
	}
end

return M
