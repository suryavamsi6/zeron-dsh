-- The main window of ui/shell_fixture.luau (its chats, projects and host
-- stubs), with SUR-83's Captain calls answered here rather than by an
-- engine: tests/captains_promote.test.luau drives SUR-88 through it.
--
-- The test sets these:
--   test_captains_roster   { [chat id] = name } — what `Captains` answers
--   test_captain_calls     what the actions were sent, in order
--   test_captains_refuse   while true, each promotion fails in the engine's
--                          own words (as a refused one does)
--   test_captains_archived { [chat id] = true } — rows the roster keeps as
--                          demoted records: the fixture answers a demote the
--                          way the engine does, so the record outlives it

local tree = require("@app/shell_fixture")
local sidebar = require("@app/screens/shell/sidebar")

local roster = ui.shared("test_captains_roster", {} :: { [string]: string })
local calls = ui.shared("test_captain_calls", "" :: string)
local refuse = ui.shared("test_captains_refuse", false)
local archived = ui.shared("test_captains_archived", {} :: { [string]: boolean })
local hold_roster = ui.shared("test_captains_hold_roster", false)
local release_roster = ui.shared("test_captains_release_roster", false)

-- Each call answered on the next tick, as the host's is. A roster read can
-- additionally be held open, so a test can put a promotion inside its
-- in-flight window as a slower engine would.
local due: { () -> () } = {}
local held_roster: { () -> () } = {}
ui.every(16, function()
	if release_roster:peek() and #held_roster > 0 then
		local now = held_roster
		held_roster = {}
		for _, settle in now do
			settle()
		end
	end
	if #due > 0 then
		local now = due
		due = {}
		for _, settle in now do
			settle()
		end
	end
	return true
end)

local function settle(value: any, err: string?): any
	return {
		await = function(_): any
			if err then
				error(err, 0)
			end
			return value
		end,
		after = function(_, ok: (any) -> (), fail: ((any) -> ())?)
			table.insert(due, function()
				if err then
					if fail then
						fail({ message = err })
					end
				else
					ok(value)
				end
			end)
		end,
	}
end

local function deferred(value: any): any
	return {
		await = function(_): any
			return value
		end,
		after = function(_, ok: (any) -> (), _fail: ((any) -> ())?)
			table.insert(held_roster, function()
				ok(value)
			end)
		end,
	}
end

local function note(what: string)
	local now = calls:peek()
	calls:set(if now == "" then what else `{now},{what}`)
end

sidebar.roster = function(): any
	local captains = {}
	for chat, name in roster:peek() do
		-- (A demoted Captain stays on the engine's roster, archived: the page
		-- reads that row as no Captain at all.)
		table.insert(captains, { chatId = chat, name = name, archived = archived:peek()[chat] == true, drivePath = `/drive/{chat}` })
	end
	local answer = { captains = captains }
	if hold_roster:peek() and not release_roster:peek() then
		return deferred(answer)
	end
	return settle(answer)
end

sidebar.promote_call = function(chat: string, name: string): any
	note(`promote:{chat}:{name}`)
	if refuse:peek() then
		return settle(nil, `bad params: {chat} is not a chat this device knows`)
	end
	roster:set_key(chat, name)
	archived:set_key(chat, nil)
	return settle({ captains = {} })
end

sidebar.demote_call = function(chat: string): any
	note(`demote:{chat}`)
	-- (As the engine's: the row stays on the roster, archived.)
	archived:set_key(chat, true)
	return settle({ captains = {} })
end

-- What the shot host drives (audit_shots over this fixture): the two dialogs,
-- the roster already naming "Fix the build" a Captain for the second so its
-- alert says which.
ui.command {
	id = "open_promote",
	title = "Open Promote to Captain",
	run = function()
		sidebar.promote("a1")
	end,
}

ui.command {
	id = "open_demote",
	title = "Open Demote to chat",
	run = function()
		roster:set_key("a1", "Billing")
		sidebar.read_captains()
		ui.after(80, function()
			sidebar.demote("a1")
		end)
	end,
}

return tree
