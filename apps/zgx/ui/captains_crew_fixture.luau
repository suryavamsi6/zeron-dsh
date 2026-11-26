-- The main window of ui/shell_fixture.luau (its chats, projects and host
-- stubs), with SUR-83's crew calls answered here rather than by an engine,
-- and SUR-90's Drive derived from the crew that answer keeps:
-- tests/captains_crew.test.luau drives SUR-91 through it.
--
-- The test sets these:
--   test_crew_captains   { [chat id] = name } — the Captains `Captains` answers
--   test_crew_members    { [chat id] = captain id } — who is in whose crew
--   test_crew_calls      what the actions were sent, in order
--   test_crew_refuse     while not "", each AddCrew fails in these words (the
--                        engine's own sentence, as a refused one does)
--   test_crew_archived   { [chat id] = true } — rows the roster keeps as
--                        demoted records, as the engine does

local tree = require("@app/shell_fixture")
local sidebar = require("@app/screens/shell/sidebar")
local CaptainDigest = require("@app/screens/chat/captain_digest")
local C = require("./captain_drive_contract")

local captains = ui.shared("test_crew_captains", {} :: { [string]: string })
local members = ui.shared("test_crew_members", {} :: { [string]: string })
local calls = ui.shared("test_crew_calls", "" :: string)
local refuse = ui.shared("test_crew_refuse", "" :: string)
local archived = ui.shared("test_crew_archived", {} :: { [string]: boolean })

-- The roster the sidebar reads as it is made: Fix the build is the Captain,
-- and no chat is in its crew yet. (The read lands before the first menu
-- opens; a state set after the mount is a menu open behind.)
captains:set({ [C.chat] = C.name })
members:set({})

-- The chats the shell fixture knows as the host's, with their sessions ready:
-- a chat with no record is drawn as loading, and a Captain's strip sits
-- inside a session that has come.
ui.shared("fixture.chats", {} :: { [string]: any }):set({
	a1 = { session = { id = "a1", state = "ready" }, messages = { { id = "a1-m", role = "assistant", text = "Fix the build" } } },
	a2 = { session = { id = "a2", state = "ready" }, messages = { { id = "a2-m", role = "assistant", text = "Write the docs" } } },
	a3 = { session = { id = "a3", state = "ready" }, messages = { { id = "a3-m", role = "assistant", text = "Tidy the tests" } } },
})

-- The chats' titles, by id, for the report a crew member files under its own
-- name (the engine names the file; the page draws what it is given).
local TITLES: { [string]: string } = {
	a1 = "Fix the build",
	a2 = "Write the docs",
	a3 = "Tidy the tests",
	b1 = "Ship the release",
	b2 = "Answer the review",
}

-- Each call answered on the next tick, as the host's is.
local due: { () -> () } = {}
ui.every(16, function()
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

local function note(what: string)
	local now = calls:peek()
	calls:set(if now == "" then what else `{now},{what}`)
end

-- The roster as the engine's `Captains` answers it: each Captain's crew, in
-- its own order.
local function roster(): any
	local out = {}
	for chat, name in captains:peek() do
		local crew = {}
		for member, captain in members:peek() do
			if captain == chat then
				table.insert(crew, member)
			end
		end
		table.sort(crew)
		table.insert(out, {
			chatId = chat,
			name = name,
			createdAt = 0,
			-- (A demoted Captain stays on the engine's roster, archived.)
			archived = archived:peek()[chat] == true,
			crew = crew,
			drivePath = `/drive/{chat}`,
		})
	end
	return { captains = out }
end

sidebar.roster = function(): any
	return settle(roster())
end

sidebar.add_crew_call = function(captain: string, chat: string): any
	note(`add:{captain}:{chat}`)
	local why = refuse:peek()
	if why ~= "" then
		return settle(nil, why)
	end
	members:set_key(chat, captain)
	return settle(roster())
end

sidebar.remove_crew_call = function(captain: string, chat: string): any
	note(`remove:{captain}:{chat}`)
	members:set_key(chat, nil)
	return settle(roster())
end

-- The Captain's Drive (SUR-90's two calls): a report for each chat in the
-- crew, under the chat's own title, so a chat just added is in the Digest
-- and a released one is gone.
CaptainDigest.roster = function(): any
	return settle(roster())
end

CaptainDigest.drive = function(chat: string): any
	local answer = C.digest_answer()
	local reports = {}
	for member, captain in members:peek() do
		if captain == chat then
			local title = TITLES[member] or member
			table.insert(reports, { name = title, text = `# Crew report\n\n{title} finished its turn.` })
		end
	end
	answer.reports = reports
	return settle(answer)
end

-- What a test drives to make the sidebar read the roster again (SUR-89's
-- group): the app re-reads it as a menu opens or an action lands, and a
-- test that sets `test_crew_captains`/`test_crew_members` after the mount
-- asks for the same read.
ui.command {
	id = "refresh_captains",
	title = "Read the Captains again",
	run = function()
		sidebar.read_captains()
	end,
}

return tree
