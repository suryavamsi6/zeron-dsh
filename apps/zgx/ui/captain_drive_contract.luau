-- The Captain's Drive as the host sends it, in one place (SUR-250).
--
-- `screens/chat/captain_drive` makes exactly two calls, both to
-- `apps/zgx/src/captains.rs`: `CaptainDrive` (`drive_files`, `:235-264`) and
-- `CaptainDriveRead` (`drive_read`, `:291-336`). A listing row is
-- `{ name, kind, size?, mtime? }` with `name` the Drive-relative path a read
-- takes, `kind` "file" or "directory", a file carrying `size` (bytes) and
-- `mtime` (RFC 3339 text, seconds, UTC, `Z`), a directory carrying neither,
-- sorted by name in byte order — the Drive's top level but `state/` (the
-- engine's own) plus the files directly under `reports/`, never that
-- folder's children nor `handoffs/`'s. A read answers `{ text, size }` and
-- nothing else; a path out of the Drive, or one not in it, is the host's
-- error, word for word.
--
-- `captain_drive_fixture.luau` answers from this, so the suite has one copy
-- of the contract rather than several; `tests/captain_drive_contract.test.luau`
-- pins the shape below, and `C.problems()` is what the fixture refuses to
-- mount over. What this file cannot see is the host changing under it — that
-- half is `captains_tests.rs`'s own.

local C = {}

-- The engine's own folder inside every Drive: the host never lists it, and
-- the page hides it even if one arrives.
C.HIDDEN = "state"

-- Every row the host sends, in its order: the Drive's top level (`state/`
-- excepted) and the files directly under `reports/`, sorted by name.
C.listing = {
	{ name = "AGENTS.md", kind = "file" },
	{ name = "CREW.md", kind = "file" },
	{ name = "DIGEST.md", kind = "file" },
	{ name = "NOTES.md", kind = "file" },
	{ name = "handoffs", kind = "directory" },
	{ name = "reports", kind = "directory" },
	{ name = "reports/a2.md", kind = "file" },
}

-- Each file's text, by the Drive-relative name a read takes.
C.texts = {
	["AGENTS.md"] = "# Billing: Captain rules\n\nThe crew never reads this file.\n",
	["CREW.md"] = "# Crew rules\n\nOpen PRs as drafts.\n",
	["DIGEST.md"] = "# Billing\n\nTwo things are waiting on you.\n",
	["NOTES.md"] = "# Plan and notes\n\nShip the invoice page.\n",
	["reports/a2.md"] = "# Crew report\n\nInvoices reconciled; one needs your approval.\n",
}

-- A file's `mtime`, as the host formats it: RFC 3339 text, seconds, UTC,
-- `Z`. (The host sends the file's own time; a fixture's files are made up,
-- so one time stands for them.)
C.mtime = "2026-11-23T15:10:41Z"

-- The Captain fixture the Digest and crew fixtures share (SUR-225): one
-- Captain, Billing, with two crew chats and the Drive path the roster carries.
-- The files above are that Drive: `DIGEST.md`, the report `reports/a2.md`,
-- and the machine-readable Needs-you snapshot below (what lives at the
-- host-hidden `state/needs-you.json`).
C.chat = "a1"
C.name = "Billing"
C.crew = { "a2", "a3" }
C.drive_path = "/drive/a1"
C.needs_you = {
	version = 1,
	updatedAt = "2026-11-22T01:00:00Z",
	items = { "Approve the invoice", "Pick a deploy window" },
}

-- The crew map the roster stands for (`member -> captain`), as a fresh table.
function C.crew_members(): { [string]: string }
	return { a2 = C.chat, a3 = C.chat }
end

-- The roster row for this Captain, as `Captains` answers it. `crew` defaults
-- to the fixture's two chats; passing one lets a crew-mutation fixture answer
-- the row as its membership changes.
function C.roster_row(crew: { string }?): { [string]: any }
	return {
		chatId = C.chat,
		name = C.name,
		createdAt = 0,
		archived = false,
		userArchived = false,
		crew = crew or { "a2", "a3" },
		drivePath = C.drive_path,
	}
end

-- The whole roster answer, one Captain on it.
function C.roster_answer(crew: { string }?): { [string]: any }
	return { captains = { C.roster_row(crew) } }
end

-- The Drive as `CaptainDrive` answers it for the Digest tab: `DIGEST.md`,
-- the Needs-you snapshot, and the crew reports newest first. Fresh tables,
-- so a fixture's end-of-turn command can write its own next state.
function C.digest_answer(): { [string]: any }
	return {
		digest = C.texts["DIGEST.md"],
		needsYou = {
			version = C.needs_you.version,
			updatedAt = C.needs_you.updatedAt,
			items = { table.unpack(C.needs_you.items) },
		},
		reports = {
			{ name = "a2.md", text = C.texts["reports/a2.md"] },
		},
	}
end

-- `CaptainDriveRead`'s answer holds these keys and no others (`drive_read`'s
-- `{ text, size }`) — a `path` is the page's own, never the host's.
C.read_keys = { "size", "text" }

-- The host's two refusals, word for word.
C.escape_refusal = "The path leaves the Captain's Drive"
C.missing_refusal = "The file isn't in the Captain's Drive"

-- The Drive's files as `CaptainDrive` answers them: a file with `size` and
-- `mtime`, a directory with neither.
function C.files(): { { name: string, kind: string, size: number?, mtime: string? } }
	local out = {}
	for _, row in C.listing do
		if row.kind == "file" then
			local text = C.texts[row.name]
			table.insert(out, { name = row.name, kind = "file", size = #text, mtime = C.mtime })
		else
			table.insert(out, { name = row.name, kind = "directory" })
		end
	end
	return out
end

-- Whether `path` is one the host refuses as out of the Drive: empty,
-- absolute, or carrying a `..` component.
local function escapes(path: string): boolean
	if path == "" or string.sub(path, 1, 1) == "/" then
		return true
	end
	for part in string.gmatch(path, "[^/]+") do
		if part == ".." then
			return true
		end
	end
	return false
end

-- What `CaptainDriveRead` answers for `path`: the two keys, or the host's
-- refusal (`drive_read`'s own words).
function C.read(path: string): (any, string?)
	local text = C.texts[path]
	if text then
		return { text = text, size = #text }
	end
	return nil, if escapes(path) then C.escape_refusal else C.missing_refusal
end

-- Everything above that the host could not send, said one per line: what the
-- fixture refuses to mount over, and what the case in
-- tests/captain_drive_contract.test.luau asserts is empty.
function C.problems(): { string }
	local out: { string } = {}
	local seen: { [string]: boolean } = {}
	local last = ""
	for _, row in C.listing do
		if type(row.name) ~= "string" or row.name == "" then
			table.insert(out, `a row with no name: {tostring(row.name)}`)
		elseif seen[row.name] then
			table.insert(out, `{row.name}: listed twice`)
		elseif row.name == C.HIDDEN or string.sub(row.name, 1, #C.HIDDEN + 1) == C.HIDDEN .. "/" then
			table.insert(out, `{row.name}: the engine's own folder is never listed`)
		elseif row.name <= last then
			table.insert(out, `{row.name}: out of the host's order (after {last})`)
		end
		last = row.name
		seen[row.name] = true
		local text = C.texts[row.name]
		local folder, below = string.match(row.name, "^([^/]+)/(.+)$")
		if row.kind == "file" then
			if not text then
				table.insert(out, `{row.name}: a file with no text to answer a read with`)
			elseif folder and (folder ~= "reports" or string.find(below, "/", 1, true)) then
				table.insert(out, `{row.name}: a file nested deeper than reports/, which the host never lists`)
			end
		elseif row.kind == "directory" then
			if text then
				table.insert(out, `{row.name}: a directory carrying a file's text`)
			end
		else
			table.insert(out, `{row.name}: kind {tostring(row.kind)}, which is neither "file" nor "directory"`)
		end
	end
	for name in C.texts do
		if not seen[name] then
			table.insert(out, `{name}: a text no listing row reads`)
		end
	end
	for _, key in C.read_keys do
		if key ~= "text" and key ~= "size" then
			table.insert(out, `a read key the host never sends: {tostring(key)}`)
		end
	end
	if #C.read_keys ~= 2 then
		table.insert(out, `{ #C.read_keys } read keys, the host's answer holds two (text, size)`)
	end
	if not string.match(C.mtime, "^%d%d%d%d%-%d%d%-%d%dT%d%d:%d%d:%d%dZ$") then
		table.insert(out, `mtime "{C.mtime}" is not RFC 3339 text in seconds, UTC`)
	end
	return out
end

return C
