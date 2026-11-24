-- SUR-251's host stand-in: the shell fixture's chats, with a Captain roster
-- and the three Captain actions (CaptainArchive / CaptainUnarchive /
-- CaptainDelete) answered here rather than by an engine — the same shape
-- SUR-94's host half answers with: `{ chatId }` in, the new roster out, a
-- refusal as a command error.
--
-- The test sets these:
--   test_sur251_asleep  { [chat id] = true } — the Captains the person put
--                        away (`userArchived`), which the roster still answers
--   test_sur251_calls   what the actions were sent, in order
--   test_sur251_refuse  while not "", each action fails in these words
--
-- The roster's fields are SUR-94's `RosterEntry` on head `c131526326`:
-- `chatId`, `name`, `createdAt` (RFC 3339 text), `archived`, `userArchived`,
-- `crew`, `drivePath` — and nothing else; `archivedCrew` is the registry's
-- own marker and never reaches the page.
--
-- Archive moves the Captain's **crew** onto the Archived shelf and leaves the
-- Captain's own chat where it is, as the host does: `Registry::archive`
-- returns the crew chats it moved and `apps/zgx/src/captains.rs` archives
-- exactly those (`set_chat_archived` over `moved` only), while `DeleteCaptain`
-- says the record's chats keep their rows. So a put-away Captain keeps an
-- ordinary row, and its menu — Unarchive Captain, Delete Captain, the chat's
-- own Archive and Delete suppressed — is reached on that row, not on the
-- shelf.

local tree = require("@app/shell_fixture")
local sidebar = require("@app/screens/shell/sidebar")
local sessions = require("@account/sessions")

local asleep = ui.shared("test_sur251_asleep", {} :: { [string]: boolean })
local calls = ui.shared("test_sur251_calls", "" :: string)
local refuse = ui.shared("test_sur251_refuse", "" :: string)
local names = ui.shared("test_sur251_names", { a1 = "Billing" } :: { [string]: string })
local members = ui.shared("test_sur251_members", { a2 = "a1" } :: { [string]: string })

-- Each answer lands on the next tick, as the host's does.
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

-- The roster as the engine answers it: live and put-away records alike, each
-- with its crew, the put-away ones carrying `userArchived`.
local function roster(): any
	local out = {}
	for chat, name in names:peek() do
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
			-- (RFC 3339 text, as `RosterEntry` serialises its `created_at`:
			-- the host sends a date, not epoch seconds.)
			createdAt = "2026-11-24T08:00:00Z",
			archived = false,
			userArchived = asleep:peek()[chat] == true,
			crew = crew,
			drivePath = `/drive/{chat}`,
		})
	end
	return { captains = out }
end

sidebar.roster = function(): any
	return settle(roster())
end

local function acted(what: string, chat: string, then_do: (() -> ())?): any
	note(`{what}:{chat}`)
	local why = refuse:peek()
	if why ~= "" then
		return settle(nil, why)
	end
	if then_do then
		then_do()
	end
	return settle(roster())
end

sidebar.archive_captain_call = function(chat: string): any
	return acted("archive", chat, function()
		asleep:set_key(chat, true)
		-- (The crew goes to the shelf and the Captain's own chat does not:
		-- `Registry::archive` returns the crew chats it moved and the host
		-- archives exactly those, while `DeleteCaptain` says the record's
		-- chats keep their rows. So the Captain keeps an ordinary row, and
		-- that row — not a shelf row — is where its menu is reached.)
		for member, captain in members:peek() do
			if captain == chat then
				sessions.archive(member)
			end
		end
	end)
end

sidebar.unarchive_captain_call = function(chat: string): any
	return acted("unarchive", chat, function()
		asleep:set_key(chat, nil)
		-- (The same set, woken: `unarchive_pending` names the crew chats.)
		for member, captain in members:peek() do
			if captain == chat then
				sessions.unarchive(member)
			end
		end
	end)
end

sidebar.delete_captain_call = function(chat: string): any
	return acted("delete", chat, function()
		names:set_key(chat, nil)
	end)
end

return tree
