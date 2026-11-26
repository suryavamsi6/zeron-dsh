-- The Captain's Drive on its own, over a made-up roster and Drive (no engine
-- in a test host): the paper the Captain and its crew keep, listed in the
-- panel's Drive tab and read in the files' own viewer.
-- ui/tests/captain_drive.test.luau drives it: the Drive's answers are the
-- host's (`src/captains.rs` `CaptainDrive` and `CaptainDriveRead`, SUR-249),
-- stood in for at screens/chat/captain_drive's two calls.
--
-- What it answers is `captain_drive_contract.luau`'s, not a second copy of it
-- (SUR-250): the listing rows, each file's text, the read's two keys and the
-- host's two refusals all come from there, so a change to the contract cannot
-- leave this fixture answering an older shape while the suite stays green.
-- `state/needs-you.json` is added on top on purpose — the host must not send
-- it, and the tab hides it even if one does.

ui.theme.strict("warn")
local ds = require("@ds")
ds.setup()

local store = require("@app/screens/chat/store")
local CaptainDrive = require("@app/screens/chat/captain_drive")
local CaptainDigest = require("@app/screens/chat/captain_digest")
local Panel = require("@app/screens/panel")

local C = require("./captain_drive_contract")

-- A contract the host could not send still mounts, so the case reads what is
-- wrong rather than dying in the harness's own mount error: the problems are
-- on `captain_drive_capture` for `tests/captain_drive_contract.test.luau` to
-- name, and on screen for whoever opens the fixture by hand.
local capture = ui.state("captain_drive_capture", nil :: any)
local problems = C.problems()
if #problems > 0 then
	capture:set({ problems = problems })
	return ui.text {
		"the Drive's contract is not what the host sends: " .. table.concat(problems, "; "),
		literal = true,
	}
end

local CHAT = C.chat
local SESSION = { id = CHAT, cwd = "/proj" }
local PROJECT = { id = "p1", path = "/proj", name = "proj" }

-- The contract's files (its listing, each file's text), which the commands
-- below file into and empty.
local texts: { [string]: string } = {}
for name, text in C.texts do
	texts[name] = text
end
local rows = C.files()
-- (The engine's own folder, which the host never lists: the tab hides it.)
table.insert(rows, { name = "state/needs-you.json", kind = "file", size = 40, mtime = C.mtime })

-- The Drive as the host lists it (SUR-249): one flat array, each `name` the
-- Drive-relative path `CaptainDriveRead` takes, sorted by name, `size` and
-- `mtime` on files only — `mtime` as RFC 3339 text, the host's own shape.
local function listing(): { { name: string, kind: string, size: number?, mtime: string? } }
	return rows
end

-- What `CaptainDriveRead` answers for `path`: the two keys the host sends, or
-- its refusal — the same answers `capture_drive` records for the cases.
local function read_answer(path: string): (any, string?)
	local text = texts[path]
	if text then
		return { text = text, size = #text }
	end
	local _, refusal = C.read(path)
	return nil, refusal
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
	return settle(C.roster_answer())
end

CaptainDrive.drive = function(_chat: string): any
	local answer = C.digest_answer()
	answer.files = listing()
	return settle(answer)
end

CaptainDrive.read = function(_chat: string, path: string): any
	local answer, refusal = read_answer(path)
	if answer then
		return settle(answer)
	end
	return failing(refusal :: string)
end

store.session:set(SESSION)
store.messages:set({ { id = "m1", role = "user", text = "What's still open?" } })

-- (What the Drive answers, as the fixture serves it, goes on the `capture`
-- above from `capture_drive` below — the stub and the command share one
-- `read_answer`.)

-- The Drive tab, opened as the panel's home opens it.
ui.command {
	id = "open_drive",
	title = "Open the Drive",
	run = function()
		Panel.open_drive(SESSION, CHAT)
	end,
}

ui.command {
	id = "capture_drive",
	title = "Capture the Drive's answers",
	run = function()
		local files = listing()
		local reads = {}
		for _, row in files do
			if row.kind == "file" and texts[row.name] then
				reads[row.name] = read_answer(row.name)
			end
		end
		local _, missing = read_answer("notes-that-are-not-there.md")
		local _, escape = read_answer("../../outside.md")
		-- (A file inside a folder by its bare name: the host takes the
		-- Drive-relative path, so this is a refusal, not that file.)
		local _, bare = read_answer("a2.md")
		capture:set({ files = files, reads = reads, missing = missing, escape = escape, bare = bare })
	end,
}

-- A Drive before the Captain's first turn: no listing at all.
ui.command {
	id = "clear_drive",
	title = "Empty the Drive",
	run = function()
		rows = {}
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
		texts["reports/a3.md"] = "# Crew report\n\nThe renewal is signed and filed.\n"
		table.insert(rows, {
			name = "reports/a3.md",
			kind = "file",
			size = #texts["reports/a3.md"],
			mtime = C.mtime,
		})
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
