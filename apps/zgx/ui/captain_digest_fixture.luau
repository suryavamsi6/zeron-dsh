-- The Captain's Digest on its own, over a made-up roster and Drive (no engine
-- in a test host): the needs-you strip above a stand-in transcript, and the
-- session's panel beside it, where "Open digest" opens the Digest tab.
-- ui/tests/captain_digest.test.luau drives it: the Drive's answers are the
-- host's (`src/captains.rs` `CaptainDrive`), stood in for at
-- screens/chat/captain_digest's two calls.

ui.theme.strict("warn")
local ds = require("@ds")
ds.setup()

local store = require("@app/screens/chat/store")
local CaptainDigest = require("@app/screens/chat/captain_digest")
local Panel = require("@app/screens/panel")

local C = require("./captain_drive_contract")

local CHAT = C.chat
local SESSION = { id = CHAT, cwd = "/proj" }
local PROJECT = { id = "p1", path = "/proj", name = "proj" }

-- The Captain's Drive, as the host would answer it: the contract's one
-- shared fixture (`DIGEST.md`, `state/needs-you.json` and `reports/a2.md`).
-- `end_turn` below changes it as the Captain's turn writing it does.
local drive = C.digest_answer()

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

CaptainDigest.roster = function(): any
	return settle(C.roster_answer())
end
CaptainDigest.drive = function(_chat: string): any
	return settle(drive)
end

store.session:set(SESSION)
store.messages:set({ { id = "m1", role = "user", text = "What's still open?" } })

-- The Captain's turn ending: the engine writes the Drive (`state/needs-you.json`
-- with the new list), then the reply lands in the chat.
ui.command {
	id = "end_turn",
	title = "End the Captain's turn",
	run = function()
		drive.digest = drive.digest .. "- Sign the renewal\n"
		local items = { table.unpack(C.needs_you.items) }
		table.insert(items, "Sign the renewal")
		drive.needsYou = {
			version = 1,
			updatedAt = "2026-11-22T01:05:00Z",
			items = items,
		}
		drive.reports = {
			{ name = "a3.md", text = "# Crew report\n\nRenewal is signed and filed." },
			{ name = "a2.md", text = C.texts["reports/a2.md"] },
		}
		store.messages:push({ id = `m{#store.messages:peek() + 1}`, role = "assistant", text = "Done — see the digest." })
	end,
}

-- Open the Digest tab from its own way, as the strip's link does (for the
-- tab's own case, with the strip hidden).
ui.command {
	id = "open_digest",
	title = "Open the Digest",
	run = function()
		Panel.open_digest(SESSION, CHAT)
	end,
}

-- A Drive before the Captain's first turn: no digest, no asks, no reports.
ui.command {
	id = "clear_drive",
	title = "Empty the Drive",
	run = function()
		drive.digest, drive.needsYou, drive.reports = nil, nil, {}
		CaptainDigest.reload(CHAT)
	end,
}

-- More asks than the strip shows: seven, for its five.
ui.command {
	id = "many_asks",
	title = "Ask seven things",
	run = function()
		local items = {}
		for i = 1, 7 do
			table.insert(items, `ask {i}`)
		end
		drive.needsYou = { version = 1, updatedAt = "2026-11-22T01:10:00Z", items = items }
		CaptainDigest.reload(CHAT)
	end,
}

return ui.row {
	w_full = true,
	h_full = true,
	ui.column {
		grow = 1,
		h_full = true,
		min_w = 0,
		CaptainDigest.Strip {
			chat = CHAT,
			open = function(chat: string)
				Panel.open_digest(SESSION, chat)
			end,
		},
		ui.column {
			id = "transcript",
			grow = 1,
			w_full = true,
			min_h = 0,
			ds.Text { "the conversation" },
		},
	},
	ui.column {
		w = 420,
		h_full = true,
		Panel.Panel {
			session = SESSION,
			project = PROJECT,
			w_full = true,
			h_full = true,
		},
	},
}
