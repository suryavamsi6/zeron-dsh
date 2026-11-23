-- A Captain's Drive, as the chat it belongs to shows it: the files the
-- Captain and its crew keep (`AGENTS.md`, `CREW.md`, `NOTES.md`,
-- `DIGEST.md`, then the four folders), each with one muted line saying what
-- it is, and a file open in the files' own viewer, to read.
--
--   CaptainDrive.Tab { chat = <id> }
--
-- The Drive is the host's to read (`apps/zgx/src/captains.rs`: the roster's
-- `drivePath` is the only path it takes), so the page names a chat, never a
-- folder. `CaptainDrive` carries the listing beside the Digest's three
-- answers; `CaptainDriveRead` is one file of it, read inside the Drive.
-- Both are on `M` so a test stands them in (ui/captain_drive_fixture.luau).
--
-- `state/` is machine-only and never listed, by the host or here. Before the
-- Captain's first turn there is no Drive, and the tab says so.

local ds = require("@ds")
local FileIcon = require("@ds/file_icons")
local FileViewer = require("@ds/file_viewer")
local ListRow = require("@ds/list_row").ListRow
local store = require("@app/screens/chat/store")

local M = {}

-- The host's listing (SUR-249): one flat array, sorted by name, each entry
-- `{ name, kind, size?, mtime? }` whose `name` is the Drive-relative path
-- `CaptainDriveRead` takes ("reports/2026-11-23-a.md", "DIGEST.md") and whose
-- `kind` is "file" or "directory". `mtime` is RFC 3339 text, as the host
-- formats it, kept as text here (never converted). The Drive's top level plus
-- `reports/`'s own files; `state/` is never in it.
export type Entry = { name: string, kind: string, size: number?, mtime: string? }
export type File = { path: string, text: string?, size: number?, truncated: boolean? }
export type Listing = { entries: { Entry } }

-- Capy's Drive table, in its order: the four files first, then anything else
-- alphabetically; the folders in the order Capy lists them.
M.FILES = { "AGENTS.md", "CREW.md", "NOTES.md", "DIGEST.md" }
M.FOLDERS = { "reports", "handoffs", "skills", "renders" }
M.HIDDEN = "state"

-- One muted line per row, from the same table (Capy's, docs.capy.ai/captains).
M.ABOUT = {
	["AGENTS.md"] = "chat.captain_drive.about_agents",
	["CREW.md"] = "chat.captain_drive.about_crew",
	["NOTES.md"] = "chat.captain_drive.about_notes",
	["DIGEST.md"] = "chat.captain_drive.about_digest",
	reports = "chat.captain_drive.about_reports",
	handoffs = "chat.captain_drive.about_handoffs",
	skills = "chat.captain_drive.about_skills",
	renders = "chat.captain_drive.about_renders",
} :: { [string]: string }

-- The host's two calls, each a promise: the Drive (its listing beside the
-- Digest's answers), and one file of it.
M.drive = function(chat: string): any
	return ui.call("CaptainDrive", { chatId = chat })
end

M.read = function(chat: string, path: string): any
	return ui.call("CaptainDriveRead", { chatId = chat, path = path })
end

-- What each chat's last read found.
M.listings = ui.state("captain_drive.listings", {} :: { [string]: Listing })

local reading: { [string]: boolean } = {}

-- The promise `make` answers, or nil when this host has no such action at
-- all (a fixture, an older host): the tab says the Drive is empty rather than
-- failing over a listing it cannot fill.
local function calling(make: () -> any): any?
	local ok, promise = pcall(make)
	return if ok then promise else nil
end

-- Read `chat`'s Drive listing again: a turn's end can write files, and the
-- tab follows it in place. One read at a time; a Drive not there yet — before
-- the Captain's first turn — is an empty listing rather than an error.
function M.reload(chat: string?)
	if not chat or chat == "" or reading[chat] then
		return
	end
	reading[chat] = true
	local answer = calling(function(): any
		return M.drive(chat)
	end)
	if not answer then
		reading[chat] = nil
		return
	end
	answer:after(function(drive: any)
		reading[chat] = nil
		local entries: { Entry } = {}
		for _, entry in (drive and drive.files) or {} do
			if type(entry) == "table" and type(entry.name) == "string" and entry.name ~= "" then
				table.insert(entries, {
					name = entry.name,
					kind = if entry.kind == "directory" then "directory" else "file",
					size = if type(entry.size) == "number" then entry.size else nil,
					mtime = if type(entry.mtime) == "string" then entry.mtime else nil,
				})
			end
		end
		M.listings:set_key(chat, { entries = entries })
	end, function()
		reading[chat] = nil
	end)
end

-- One row of the tab: a file or a folder, and how deep it stands (a folder's
-- own files one step in). `path` is what the row reads when it is opened —
-- the Drive-relative path the host takes, which is not `entry.name` for a
-- file inside a folder (that one shows its own name and keeps its folder in
-- its path).
export type Row = { entry: Entry, path: string, depth: number, about: string? }

-- The listing as the tab draws it: `M.FILES` in their order and then any
-- other file at the Drive's root, then each folder with its own files under
-- it (a folder with nothing in it still shows — it is there). `state/` and
-- anything under it never show.
function M.rows(listing: Listing?): { Row }
	local files: { [string]: Entry } = {}
	local folders: { [string]: Entry } = {}
	local children: { [string]: { { entry: Entry, path: string } } } = {}
	for _, entry in (listing and listing.entries) or {} do
		local path = entry.name
		local folder, below = string.match(path, "^([^/]+)/(.+)$")
		if folder then
			if folder ~= M.HIDDEN and below ~= "" then
				local list = children[folder]
				if not list then
					list = {}
					children[folder] = list
				end
				-- (Its path stays the Drive's; its name is what the row shows.)
				table.insert(list, { entry = { name = below, kind = entry.kind, size = entry.size, mtime = entry.mtime }, path = path })
			end
		elseif entry.kind == "directory" then
			if entry.name ~= M.HIDDEN then
				folders[entry.name] = entry
			end
		else
			files[entry.name] = entry
		end
	end
	local rows: { Row } = {}
	local known: { [string]: boolean } = {}
	for _, name in M.FILES do
		local entry = files[name]
		if entry then
			table.insert(rows, { entry = entry, path = entry.name, depth = 0, about = M.ABOUT[name] })
			known[name] = true
		end
	end
	local others: { string } = {}
	for name in files do
		if not known[name] then
			table.insert(others, name)
		end
	end
	table.sort(others)
	for _, name in others do
		table.insert(rows, { entry = files[name], path = files[name].name, depth = 0, about = M.ABOUT[name] })
	end
	local order: { string } = {}
	local placed: { [string]: boolean } = {}
	for _, name in M.FOLDERS do
		if folders[name] or children[name] then
			table.insert(order, name)
			placed[name] = true
		end
	end
	local more: { string } = {}
	for name in folders do
		if not placed[name] then
			table.insert(more, name)
		end
	end
	table.sort(more)
	for _, name in more do
		table.insert(order, name)
	end
	for _, name in order do
		table.insert(rows, {
			entry = folders[name] or { name = name, kind = "directory" },
			path = name,
			depth = 0,
			about = M.ABOUT[name],
		})
		local inner = children[name] or {}
		table.sort(inner, function(a: any, b: any): boolean
			return a.entry.name < b.entry.name
		end)
		for _, child in inner do
			table.insert(rows, { entry = child.entry, path = child.path, depth = 1, about = nil })
		end
	end
	return rows
end

-- The chat's id, from a string (the tab's).
local function chat_of(chat: any): string?
	local found = if type(chat) == "function" then chat() else chat
	return if type(found) == "string" and found ~= "" then found else nil
end

-- A turn's end is a new message in the chat (and, for the Captain, files
-- written): both views read again as the messages change, or as a running
-- turn ends.
local function follow(chat: string?)
	ui.effect(function()
		local _ = #store.messages:get()
		local session = store.session:get()
		local _ = if session then session.running else nil
		M.reload(chat)
	end)
end

export type TabProps = { chat: string? }

-- The Drive tab: the list, and a file of it open in the files' viewer.
M.Tab = ui.component(function(props: TabProps): Node
	local chat = props.chat or ""
	follow(if chat ~= "" then chat else nil)
	local now = ui.computed(function(): Listing?
		return M.listings:get()[chat]
	end)
	-- The file open (its Drive path, "" for the list).
	local open = ui.use_state("")
	-- What the viewer shows: a doc of the files' own shape (ds/file_viewer).
	local doc = ui.use_state(nil :: any)

	local function opening(path: string)
		open:set(path)
		doc:set({ path = path, state = "loading" })
		local answer = calling(function(): any
			return M.read(chat, path)
		end)
		if not answer then
			doc:set({
				path = path,
				state = "error",
				error = ui.t("chat.captain_drive.unreadable"):get(),
			})
			return
		end
		answer:after(function(file: any)
			if file and type(file.text) == "string" then
				doc:set({
					path = path,
					state = "ready",
					text = file.text,
					size = if type(file.size) == "number" then file.size else nil,
				})
			else
				doc:set({
					path = path,
					state = "error",
					error = ui.t("chat.captain_drive.unreadable"):get(),
				})
			end
		end, function()
			doc:set({
				path = path,
				state = "error",
				error = ui.t("chat.captain_drive.unreadable"):get(),
			})
		end)
	end

	local function row_node(row: Row): Node
		local entry = row.entry
		local about = row.about
		local label = ui.column {
			grow = 1,
			min_w = 0,
			gap = ui.raw(1, "a row's name and its one caption line"),
			ds.Text { entry.name, style = "label", tone = "primary", wrap = false },
			if about
				then ds.Text {
					function(): string
						return ui.t(about :: string):get()
					end,
					style = "caption",
					tone = "tertiary",
					wrap = true,
				}
				else nil,
		}
		local line = ui.row {
			items = "center",
			gap = "$space.2",
			px = "$space.3",
			py = "$space.2",
			pl = if row.depth > 0 then "$space.6" else "$space.3",
			FileIcon { name = entry.name, dir = entry.kind == "directory", size = 16 },
			label :: any,
		}
		if entry.kind == "directory" then
			return ui.column { w_full = true, shrink = 0, line }
		end
		return ListRow {
			label = entry.name,
			on_click = function()
				opening(entry.name)
			end,
			line,
		}
	end

	-- (The listing's own identity: a turn's end rewriting the Drive rebuilds
	-- the rows, as the Digest tab's key does.)
	local function listing_key(): string
		local listing = now:get()
		local parts: { string } = {}
		for _, entry in (listing and listing.entries) or {} do
			table.insert(parts, `{entry.name}:{entry.kind}`)
		end
		return table.concat(parts, "\u{1}")
	end

	local function listing_node(): Node
		local rows = M.rows(now:get())
		if #rows == 0 then
			return ds.EmptyState {
				icon = "folder",
				size = "sm",
				fill = true,
				title = ui.t("chat.captain_drive.empty_title"),
				body = ui.t("chat.captain_drive.empty_body"),
			}
		end
		local made: { Node } = {}
		for _, row in rows do
			table.insert(made, row_node(row))
		end
		return ui.scroll {
			grow = 1,
			w_full = true,
			min_h = 0,
			fade = { 12, 12 },
			fade_overflow = true,
			ui.column { w_full = true, py = "$space.2", table.unpack(made) },
		}
	end

	local function file_node(path: string): Node
		return ui.column {
			w_full = true,
			h_full = true,
			min_h = 0,
			ui.row {
				w_full = true,
				h = 32,
				shrink = 0,
				items = "center",
				px = "$space.3",
				gap = "$space.2",
				border_sides = { 0, 0, 1, 0 },
				border_color = "$border.subtle",
				ds.Link {
					ui.t("chat.captain_drive.back"),
					size = "sm",
					tone = "quiet",
					on_click = function()
						open:set("")
					end,
				},
				ds.Text { string.match(path, "[^/]+$") or path, style = "label", tone = "tertiary", grow = 1 },
			},
			ui.column {
				grow = 1,
				w_full = true,
				min_h = 0,
				FileViewer { doc = doc, readonly = true },
			},
		}
	end

	return ui.column {
		id = "captain-drive",
		hook = "captain.drive",
		w_full = true,
		h_full = true,
		min_h = 0,
		ui.presence(function(): string
			local path = open:get()
			return if path == "" then `list\u{1}{listing_key()}` else `file\u{1}{path}`
		end, function(key: string): Node
			local path = string.match(key, "^file\u{1}(.*)$")
			return if path then file_node(path) else listing_node()
		end),
	}
end)

return M
