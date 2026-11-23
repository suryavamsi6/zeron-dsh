-- A Captain's Digest, as the chat it belongs to shows it: the needs-you
-- strip over its transcript, and the Digest tab in the panel.
--
--   CaptainDigest.Strip { chat = <id or a function of it>, open = fn }
--   CaptainDigest.Tab { chat = <id> }
--
-- The Drive is the host's to read (`apps/zgx/src/captains.rs`'s
-- `CaptainDrive`: the roster's `drivePath`, `DIGEST.md`, the engine's
-- `state/needs-you.json` and the crew's `reports/*.md`), so the page names a
-- chat and never a path. `roster` and `drive` below are the two calls, kept
-- on `M` so a test stands them in (ui/captain_digest_fixture.luau).
--
-- A Captain's turn ends by writing the Drive (`crates/engine/src/needs_you.rs`)
-- and landing its reply in the chat: the tab and the strip read the Drive
-- again as the chat's messages change, in place.

local ds = require("@ds")
local Markdown = require("@ds/markdown")
local store = require("@app/screens/chat/store")

local M = {}

export type Captain = { chatId: string, name: string, drivePath: string? }
export type NeedsYou = { version: number?, updatedAt: string?, items: { string } }
export type Report = { name: string, text: string }
export type Drive = { digest: string?, needsYou: NeedsYou?, reports: { Report } }

-- The strip lists at most this many of what the Captain asks (the engine's
-- own `state/needs-you.json` is a digest's list, capped at 50); the tab
-- shows this many of the crew's newest reports.
M.ASKS = 5
M.REPORTS = 3

-- The two host calls, each a promise: the roster (`Captains`), and one
-- Captain's Drive (`CaptainDrive`).
M.roster = function(): any
	return ui.call("Captains", {})
end

M.drive = function(chat: string): any
	return ui.call("CaptainDrive", { chatId = chat })
end

-- What each chat's last read found: whether it is a Captain, and its Drive.
M.captains = ui.state("captain_digest.captains", {} :: { [string]: Captain })
M.drives = ui.state("captain_digest.drives", {} :: { [string]: Drive })

local reading: { [string]: boolean } = {}
local EMPTY: Drive = { reports = {} }

-- The promise `make` answers, or nil when this host has no such action at all
-- (`ui.call` raises for a host action it does not know): a page mounted
-- without the app's host — a fixture, an older host — draws the chat as no
-- Captain rather than failing over a strip it cannot fill.
local function calling(make: () -> any): any?
	local ok, promise = pcall(make)
	return if ok then promise else nil
end

-- Read `chat`'s Drive again: whether it is a Captain (a chat that is not is
-- drawn as nothing at all), and then what the Drive holds. One read of each
-- at a time; a Drive not there yet — before the Captain's first turn — is
-- empty rather than an error.
function M.reload(chat: string?)
	if not chat or chat == "" or reading[chat] then
		return
	end
	reading[chat] = true
	local roster = calling(M.roster)
	if not roster then
		reading[chat] = nil
		return
	end
	roster:after(function(answer: any)
		local captain: Captain? = nil
		for _, found in (answer and answer.captains) or {} do
			if found.chatId == chat then
				captain = found
				break
			end
		end
		if not captain then
			reading[chat] = nil
			M.captains:set_key(chat, nil)
			M.drives:set_key(chat, nil)
			return
		end
		M.captains:set_key(chat, captain)
		local answer = calling(function(): any
			return M.drive(chat)
		end)
		if not answer then
			reading[chat] = nil
			M.drives:set_key(chat, EMPTY)
			return
		end
		answer:after(function(drive: any)
			reading[chat] = nil
			M.drives:set_key(chat, {
				digest = if drive then drive.digest else nil,
				needsYou = if drive then drive.needsYou else nil,
				reports = if drive and drive.reports then drive.reports else {},
			})
		end, function()
			reading[chat] = nil
			M.drives:set_key(chat, EMPTY)
		end)
	end, function()
		reading[chat] = nil
	end)
end

-- The asks the strip shows: the first `M.ASKS`, blanks and anything that is
-- not a line dropped.
local function asks_of(drive: Drive?): { string }
	local out: { string } = {}
	for _, item in (drive and drive.needsYou and drive.needsYou.items) or {} do
		if type(item) == "string" and item ~= "" then
			table.insert(out, item)
			if #out == M.ASKS then
				break
			end
		end
	end
	return out
end

M.asks_of = asks_of

-- The chat's id, from a string or a function of it (the page's session can
-- change under a strip).
local function chat_of(chat: any): string?
	local found = if type(chat) == "function" then chat() else chat
	return if type(found) == "string" and found ~= "" then found else nil
end

-- A turn's end is a new message in the chat (and, for the Captain, its Drive
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

export type StripProps = {
	chat: string | (() -> string?),
	-- The Digest tab, opened on that chat (the page's own panel).
	open: ((chat: string) -> ())?,
}

-- Over the transcript: a single hairline's strip of what the Captain asks
-- you for, and the way to its Digest. Nothing at all until the chat is a
-- Captain with something asked — before its first turn there is no Drive and
-- no list.
M.Strip = ui.component(function(props: StripProps): Node
	follow(chat_of(props.chat))
	-- (The read as it lands: the strip's own props do not change.)
	local now = ui.computed(function(): { captain: Captain?, asks: { string } }
		local chat = chat_of(props.chat)
		return {
			captain = if chat then M.captains:get()[chat] else nil,
			asks = asks_of(if chat then M.drives:get()[chat] else nil),
		}
	end)
	return ui.column {
		id = "captain-needs-you",
		w_full = true,
		shrink = 0,
		hidden = function(): boolean
			local found = now:get()
			return found.captain == nil or #found.asks == 0
		end,
		px = "$space.4",
		py = "$space.2",
		gap = "$space.1",
		border_sides = { 0, 0, 1, 0 },
		border_color = "$border.subtle",
		ui.row {
			w_full = true,
			items = "center",
			gap = "$space.2",
			ui.div { shrink = 0, color = "$accent", ds.Icon { name = "bell", size = 12 } },
			ds.Text {
				function(): string
					return ui.t("chat.captain_digest.needs_you", { n = #now:get().asks }):get()
				end,
				style = "label",
				tone = "secondary",
				grow = 1,
			},
			ds.Link {
				ui.t("chat.captain_digest.open"),
				size = "sm",
				tone = "quiet",
				on_click = function()
					local chat = chat_of(props.chat)
					if props.open and chat then
						props.open(chat)
					end
				end,
			},
		},
		-- (The asks as they are now: a turn's end rewrites the list, and the
		-- rows are made again.)
		ui.presence(function(): string
			return table.concat(now:get().asks, "\n")
		end, function(_key: string): Node
			local rows: { Node } = {}
			for _, ask in now:get().asks do
				table.insert(rows, ui.row {
					w_full = true,
					items = "center",
					gap = "$space.2",
					pl = "$space.4",
					ui.div { w = 4, h = 4, shrink = 0, rounded = "$radius.full", bg = "$accent" },
					ds.Text { ask, style = "caption", tone = "primary", grow = 1, wrap = true },
				})
			end
			return ui.column { w_full = true, gap = "$space.1", table.unpack(rows) }
		end),
	}
end)

export type TabProps = { chat: string? }

-- The Digest tab: `DIGEST.md` in the message markdown's own renderer, then
-- the crew's newest reports, each with the name it filed under.
M.Tab = ui.component(function(props: TabProps): Node
	local chat = props.chat or ""
	follow(if chat ~= "" then chat else nil)
	local now = ui.computed(function(): Drive?
		return M.drives:get()[chat]
	end)
	local function key(): string
		local drive = now:get()
		if not drive then
			return ""
		end
		-- (Its words and its reports' both: a turn's end changing either
		-- draws them again.)
		local parts: { string } = { drive.digest or "" }
		for _, report in drive.reports do
			table.insert(parts, report.name .. ":" .. report.text)
		end
		return table.concat(parts, "\u{1}")
	end
	return ui.column {
		id = "captain-digest",
		hook = "captain.digest",
		w_full = true,
		h_full = true,
		min_h = 0,
		ui.scroll {
			grow = 1,
			w_full = true,
			min_h = 0,
			fade = { 12, 12 },
			fade_overflow = true,
			ui.column {
				w_full = true,
				px = "$space.5",
				py = "$space.4",
				gap = "$space.4",
				ui.presence(key, function(_key: string): Node
					local drive = now:get()
					local children: { Node } = {}
					if not drive or drive.digest == nil or drive.digest == "" then
						table.insert(children, ds.EmptyState {
							icon = "document",
							size = "sm",
							fill = true,
							title = ui.t("chat.captain_digest.empty_title"),
							body = ui.t("chat.captain_digest.empty_body"),
						})
					else
						table.insert(children, Markdown { text = drive.digest })
						local reports: { Report } = {}
						for i, report in drive.reports do
							if i > M.REPORTS then
								break
							end
							table.insert(reports, report)
						end
						if #reports > 0 then
							table.insert(children, ds.Divider { label = ui.t("chat.captain_digest.reports") })
							for _, report in reports do
								table.insert(children, ui.column {
									w_full = true,
									gap = "$space.2",
									ds.Text { report.name, style = "label", tone = "tertiary" },
									Markdown { text = report.text },
								})
							end
						end
					end
					return ui.column { w_full = true, gap = "$space.4", table.unpack(children) }
				end),
			},
		},
	}
end)

return M
