-- Settings → Automation tasks (screens/settings/schedules): the engine's
-- scheduled tasks listed with each rule in words, made and edited on the
-- sheet, paused or resumed, run now and deleted — a delete asked first.
-- On the shell in ui/shell_fixture.luau, `Schedules`, `ScheduleSave`,
-- `ScheduleDelete` and `ScheduleRunNow` answered by ui/settings_stub.luau
-- (`settings_stub_schedule_tasks`, `settings_stub_schedule_save`/`_delete`/
-- `_run`; the ops asked for, in order, in `settings_stub_schedule_calls`).

local PAGE = "#settings-page"
local NAV = "#settings-nav"

local function open(answer: any): any
	local app = mount("shell_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	if answer ~= nil then
		app:set_state("settings_stub_schedule_tasks", answer)
	end
	app:command("shell.settings")
	app:frames(10)
	app:click(`{NAV} >> "Automation tasks"`)
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

local function ops(app: any): { any }
	return (app:state("settings_stub_schedule_calls") :: any) or {}
end

local function saves(app: any): { any }
	local out = {}
	for _, o in ops(app) do
		if o.op == "save" then
			table.insert(out, o.task)
		end
	end
	return out
end

local function deletes(app: any): { any }
	local out = {}
	for _, o in ops(app) do
		if o.op == "delete" then
			table.insert(out, o.id)
		end
	end
	return out
end

local function runs(app: any): { any }
	local out = {}
	for _, o in ops(app) do
		if o.op == "run" then
			table.insert(out, o.id)
		end
	end
	return out
end

test("Automation tasks lists each task's name, when it runs, its next run, and whether it's on", function()
	local app = open()
	expect(app:has(`{PAGE} >> "Standup summary"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Active"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Weekly digest"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Paused"`)).to_be(true)
	local tree = app:tree(PAGE)
	expect(tree).to_contain("Every day at 09:00 UTC")
	expect(tree).to_contain("Next")
	expect(tree).to_contain("Every Mon at 07:30 UTC")
	-- Asked once, as the page opened.
	expect(asked(app, "Schedules")).to_be(1)
end)

test("Automation tasks says when there are none", function()
	local app = open({ tasks = {} })
	expect(app:has(`{PAGE} >> "No automation tasks yet"`)).to_be(true)
end)

test("Automation tasks writes a daily 09:00 task's rule as the engine reads it", function()
	local app = open()
	app:click(`{PAGE} >> role:button "New task"`)
	app:frames(10)
	local sheet = `role:dialog "New task"`
	app:fill(`{sheet} >> role:textbox "Name"`, "Standup")
	app:fill(`{sheet} >> role:textbox "Instruction"`, "Summarize yesterday")
	-- A new chat each run, at 09:00 UTC.
	app:click(`{sheet} >> role:button "Run in"`)
	app:frames(10)
	app:click(`role:menuitem "A new chat each time"`)
	app:frames(10)
	app:click(`{sheet} >> role:button "Zone"`)
	app:frames(10)
	app:click(`role:menuitem "UTC"`)
	app:frames(10)
	app:click(`{sheet} >> role:button "Save task"`)
	app:frames(30)
	local wrote = saves(app)
	expect(#wrote).to_be(1)
	local task = wrote[1]
	-- Creating: no id sent (the engine gives one).
	expect(task.id).to_be(nil)
	expect(task.title).to_be("Standup")
	expect(task.prompt).to_be("Summarize yesterday")
	expect(task.target.kind).to_be("new")
	expect(task.active).to_be(true)
	expect(task.rrule:find("T090000Z\nRRULE:FREQ=DAILY;BYHOUR=9;BYMINUTE=0", 1, true)).never.to_be_nil()
	expect(task.rrule:find("^DTSTART:%d%d%d%d%d%d%d%dT090000Z")).never.to_be_nil()
	-- The list asked again, and the new task is on it.
	expect(asked(app, "Schedules")).to_be(2)
	expect(app:has(`{PAGE} >> "Standup"`)).to_be(true)
end)

test("Automation tasks writes a once task's rule with COUNT=1", function()
	local app = open()
	app:click(`{PAGE} >> role:button "New task"`)
	app:frames(10)
	local sheet = `role:dialog "New task"`
	app:fill(`{sheet} >> role:textbox "Name"`, "One off")
	app:fill(`{sheet} >> role:textbox "Instruction"`, "Remind me once")
	app:click(`{sheet} >> role:button "Run time"`)
	app:frames(10)
	app:click(`role:menuitem "Once"`)
	app:frames(10)
	app:fill(`{sheet} >> role:textbox "Date"`, "2099-01-05")
	app:fill(`{sheet} >> role:textbox "Time"`, "17:45")
	app:click(`{sheet} >> role:button "Zone"`)
	app:frames(10)
	app:click(`role:menuitem "UTC"`)
	app:frames(10)
	app:click(`{sheet} >> role:button "Save task"`)
	app:frames(30)
	local wrote = saves(app)
	expect(#wrote).to_be(1)
	expect(wrote[1].rrule).to_be("DTSTART:20990105T174500Z\nRRULE:FREQ=DAILY;COUNT=1")
end)

test("Automation tasks edits keep a task's id, its target and the chat a new_once made", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Edit Weekly digest"`)
	app:frames(10)
	local sheet = `role:dialog "Edit task"`
	app:fill(`{sheet} >> role:textbox "Name"`, "Week notes")
	app:click(`{sheet} >> role:button "Save task"`)
	app:frames(30)
	local wrote = saves(app)
	expect(#wrote).to_be(1)
	local task = wrote[1]
	expect(task.id).to_be("task_paused")
	expect(task.title).to_be("Week notes")
	expect(task.active).to_be(false)
	-- Its new_once target's folder and the chat it made are sent back.
	expect(task.target.kind).to_be("new_once")
	expect(task.target.cwd).to_be("/work/atlas")
	expect(task.target.session).to_be("a2")
	expect(task.rrule:find("T073000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO;BYHOUR=7;BYMINUTE=30", 1, true)).never.to_be_nil()
end)

test("Automation tasks pauses and resumes a task, the rest of it unchanged", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Pause"`)
	app:frames(30)
	local wrote = saves(app)
	expect(#wrote).to_be(1)
	expect(wrote[1].id).to_be("task_daily")
	expect(wrote[1].active).to_be(false)
	-- Rule, target and prompt sent back as they were.
	expect(wrote[1].rrule).to_be("DTSTART:20261101T090000Z\nRRULE:FREQ=DAILY;BYHOUR=9;BYMINUTE=0")
	expect(wrote[1].target.session).to_be("a1")
	app:frames(10)
	app:click(`{PAGE} >> role:button "Resume"[1]`)
	app:frames(30)
	wrote = saves(app)
	expect(#wrote).to_be(2)
	expect(wrote[2].id).to_be("task_daily")
	expect(wrote[2].active).to_be(true)
end)

test("Automation tasks runs a paused task now, its pause kept", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Run now"[2]`)
	app:frames(30)
	expect(runs(app)).to_equal({ "task_paused" })
	-- Still paused afterwards: run now doesn't rewrite the task.
	expect(app:has(`{PAGE} >> "Paused"`)).to_be(true)
	expect(#saves(app)).to_be(0)
end)

test("Automation tasks asks before a delete, then deletes", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Delete Standup summary"`)
	app:frames(10)
	expect(app:has(`role:dialog "Delete “Standup summary”?"`)).to_be(true)
	-- Asked first: nothing sent.
	expect(#ops(app)).to_be(0)
	app:click(`role:dialog "Delete “Standup summary”?" >> role:button "Cancel"`)
	app:frames(10)
	expect(#deletes(app)).to_be(0)
	app:click(`{PAGE} >> role:button "Delete Standup summary"`)
	app:frames(10)
	app:click(`role:dialog "Delete “Standup summary”?" >> role:button "Delete task"`)
	app:frames(30)
	expect(deletes(app)).to_equal({ "task_daily" })
	expect(app:has(`{PAGE} >> "Standup summary"`)).to_be(false)
end)

test("Automation tasks keeps the sheet open on a refused save, the refusal's words on it", function()
	local app = open()
	app:set_state("settings_stub_schedule_save", "rrule has no occurrence in the future")
	app:click(`{PAGE} >> role:button "New task"`)
	app:frames(10)
	local sheet = `role:dialog "New task"`
	app:fill(`{sheet} >> role:textbox "Name"`, "Standup")
	app:fill(`{sheet} >> role:textbox "Instruction"`, "Summarize yesterday")
	app:click(`{sheet} >> role:button "Save task"`)
	app:frames(30)
	-- Still open, its fields as typed, the engine's words shown.
	expect(app:has(sheet)).to_be(true)
	expect(app:has(`{sheet} >> "rrule has no occurrence in the future"`)).to_be(true)
	expect(#saves(app)).to_be(1)
end)
