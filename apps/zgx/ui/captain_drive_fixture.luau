-- The Captain's Drive on its own, over a made-up roster and Drive (no engine
-- in a test host): the paper the Captain and its crew keep, listed in the
-- panel's Drive tab and read in the files' own viewer.
-- ui/tests/captain_drive.test.luau drives it: the Drive's answers are the
-- host's (`src/captains.rs` `CaptainDrive` and `CaptainDriveRead`, SUR-249),
-- stood in for at screens/chat/captain_drive's two calls.

ui.theme.strict("warn")
local ds = require("@ds")
ds.setup()

local store = require("@app/screens/chat/store")
local CaptainDrive = require("@app/screens/chat/captain_drive")
local CaptainDigest = require("@app/screens/chat/captain_digest")
local Panel = require("@app/screens/panel")

local CHAT = "cap-a"
local SESSION = { id = CHAT, cwd = "/proj" }
local PROJECT = { id = "p1", path = "/proj", name = "proj" }

-- The Drive's files, as the host reads them back (`CaptainDriveRead`).
local texts: { [string]: string } = {
	["AGENTS.md"] = "# Billing: Captain rules\n\nThe crew never reads this file.\n",
	["CREW.md"] = "# Crew rules\n\nOpen PRs as drafts.\n",
	["NOTES.md"] = "# Plan and notes\n\nShip the invoice page.\n",
	["DIGEST.md"] = "# Billing\n\nTwo things are waiting on you.\n",
	["reports/2026-11-23-a.md"] = "# Crew report\n\nInvoices reconciled.\n",
}

-- The Drive as the host lists it (SUR-249): one flat array, each `name` the
-- Drive-relative path `CaptainDriveRead` takes, sorted by name, `size` and
-- `mtime` on files only — `mtime` as RFC 3339 text, the host's own shape. `state/needs-you.json` is listed here on purpose:
-- the host must not send it, and the tab hides it even if one does.
local order = {
	"AGENTS.md",
	"CREW.md",
	"DIGEST.md",
	"NOTES.md",
	"handoffs",
	"reports",
	"reports/2026-11-23-a.md",
	"state/needs-you.json",
}

local function listing(): { { name: string, kind: string, size: number?, mtime: string? } }
	local out = {}
	for _, name in order do
		local text = texts[name]
		if text then
			table.insert(out, { name = name, kind = "file", size = #text, mtime = "2026-11-23T15:10:41Z" })
		elseif name == "state/needs-you.json" then
			table.insert(out, { name = name, kind = "file", size = 40, mtime = "2026-11-23T15:10:41Z" })
		else
			table.insert(out, { name = name, kind = "directory" })
		end
	end
	return out
end

-- Each call answered on the next tick, as the host's is.
local function settle(value: any): any
	return {
		await = function(_): any
			return value
		end,
		after = function(_, ok: (any) -> (), _fail: ((any) -> ())?)
			ui.after(16, function()
				ok(value)
			end)
		end,
	}
end

-- A call the host refuses: its error, not a value (`CaptainDriveRead` on a
-- path that escapes the Drive).
local function failing(message: string): any
	return {
		await = function(_): any
			return nil
		end,
		after = function(_, _ok: (any) -> (), fail: ((any) -> ())?)
			ui.after(16, function()
				if fail then
					fail({ message = message })
				end
			end)
		end,
	}
end

CaptainDigest.roster = function(): any
	return settle { captains = { { chatId = CHAT, name = "Billing", drivePath = "/drive" } } }
end

CaptainDrive.drive = function(_chat: string): any
	return settle { files = listing(), digest = "# Billing", needsYou = nil, reports = {} }
end

CaptainDrive.read = function(_chat: string, path: string): any
	local text = texts[path]
	if not text then
		return failing("That file is not in the Captain's Drive")
	end
	return settle { path = path, text = text, size = #text }
end

store.session:set(SESSION)
store.messages:set({ { id = "m1", role = "user", text = "What's still open?" } })

-- The Drive tab, opened as the panel's home opens it.
ui.command {
	id = "open_drive",
	title = "Open the Drive",
	run = function()
		Panel.open_drive(SESSION, CHAT)
	end,
}

-- A Drive before the Captain's first turn: no listing at all.
ui.command {
	id = "clear_drive",
	title = "Empty the Drive",
	run = function()
		order = {}
		texts = {}
		CaptainDrive.reload(CHAT)
	end,
}

-- A crew's turn ending: it files a report, and the tab shows it without
-- being reopened.
ui.command {
	id = "file_report",
	title = "File another report",
	run = function()
		texts["reports/2026-11-23-b.md"] = "# Crew report\n\nThe renewal is signed and filed.\n"
		table.insert(order, "reports/2026-11-23-b.md")
		store.messages:push({ id = `m{#store.messages:peek() + 1}`, role = "assistant", text = "Filed the renewal report." })
		CaptainDrive.reload(CHAT)
	end,
}

return ui.column {
	w_full = true,
	h_full = true,
	Panel.Panel {
		session = SESSION,
		project = PROJECT,
		w_full = true,
		h_full = true,
	},
}
