-- A Captain's Drive (screens/chat/captain_drive): the tab its chat opens from
-- the panel's home, the files the Captain and its crew keep listed in Capy's
-- order with one muted line each, any of them read in the files' own viewer,
-- and the list read again as a crew's turn files something — in place.
-- ui/captain_drive_fixture.luau stands the host's two answers in (SUR-249's
-- `CaptainDrive.files` and `CaptainDriveRead`).

-- `selector`'s subtree holding `text`, waited for as the made-up host's
-- answers arrive a tick at a time.
local function until_text(app: any, selector: string, text: string, ms: number?): boolean
	for _ = 1, math.ceil((ms or 15000) / 50) do
		if string.find(app:tree(selector), text, 1, true) then
			return true
		end
		app:wait('"never shown, only waited past"', { timeout_ms = 50 })
	end
	return string.find(app:tree(selector), text, 1, true) ~= nil
end

local function home(app: any)
	app:resize(1100, 780)
	app:frames(30)
end

-- The session's panel tabs, in order ("captain_drive").
local function kinds(app: any, id: string): string
	local s = ((app:state("panel_sessions") :: any) or {})[`chat:{id}`]
	local out: { string } = {}
	for _, tab in (if s then s.tabs else {}) do
		table.insert(out, tab.kind)
	end
	return table.concat(out, ",")
end

-- Where `needle` first stands in `text` (1-based), for the rows' order.
local function at(text: string, needle: string): number
	return (string.find(text, needle, 1, true)) or 0
end

test("captain_drive: the panel's home opens the Drive tab, listing the files in Capy's order", function()
	local app = mount("captain_drive_fixture.luau")
	home(app)
	-- A Captain's chat: the panel's home offers its Drive beside the files.
	expect(app:wait("#panel-open-captain-drive", { timeout_ms = 15000 })).to_be(true)
	app:click("#panel-open-captain-drive")
	expect(app:wait("#captain-drive", { timeout_ms = 15000 })).to_be(true)
	expect(kinds(app, "cap-a")).to_be("captain_drive")
	-- The four files, then the folders, each with the line that says what it
	-- is (Capy's Drive table).
	expect(until_text(app, "#captain-drive", "AGENTS.md")).to_be(true)
	local tree = app:tree("#captain-drive")
	for _, name in { "AGENTS.md", "CREW.md", "NOTES.md", "DIGEST.md", "2026-11-23-a.md", "handoffs" } do
		expect(tree).to_contain(name)
	end
	-- (Its words on one line: as much of each as the row shows.)
	expect(tree).to_contain("The Captain's own rules")
	expect(tree).to_contain("Rules every crew chat reads")
	expect(tree).to_contain("Reports the crew leaves for the Captain.")
	-- `state/` is machine-only: not a row, not named anywhere.
	expect(tree).never.to_contain("state/")
	expect(tree).never.to_contain("needs-you.json")
	-- Its order: the four files, then reports (with the crew's report under
	-- it), then handoffs.
	local order = {
		"AGENTS.md",
		"CREW.md",
		"NOTES.md",
		"DIGEST.md",
		"Reports the crew leaves for the Captain.",
		"2026-11-23-a.md",
		"handoffs",
	}
	local last = 0
	for _, name in order do
		local found = at(tree, name)
		expect(found > 0).to_be(true)
		expect(found > last).to_be(true)
		last = found
	end
end)

test("captain_drive: a file opens in the files' viewer, read-only, and the list comes back", function()
	local app = mount("captain_drive_fixture.luau")
	home(app)
	app:command("open_drive")
	expect(app:wait("#captain-drive", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-drive", "DIGEST.md")).to_be(true)
	-- Opened: its words, in the viewer the files screen uses.
	app:click('#captain-drive >> role:button "DIGEST.md"')
	expect(until_text(app, "#captain-drive", "Two things are waiting on you.")).to_be(true)
	expect(app:tree("#captain-drive")).to_contain("All files")
	-- Nothing can be typed into it (the viewer is read-only): no Save, no
	-- editor.
	expect(app:tree("#captain-drive")).never.to_contain("Save")
	-- Back to the list.
	app:click('#captain-drive >> role:link "All files"')
	expect(until_text(app, "#captain-drive", "AGENTS.md")).to_be(true)
end)

test("captain_drive: before the Captain's first turn the tab says there is no Drive yet", function()
	local app = mount("captain_drive_fixture.luau")
	home(app)
	app:command("clear_drive")
	app:command("open_drive")
	expect(app:wait("#captain-drive", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-drive", "No Drive yet")).to_be(true)
	expect(app:tree("#captain-drive")).to_contain("The Captain makes its Drive")
	expect(kinds(app, "cap-a")).to_be("captain_drive")
end)

test("captain_drive: a crew's report filed at a turn's end shows without the tab being reopened", function()
	local app = mount("captain_drive_fixture.luau")
	home(app)
	app:command("open_drive")
	expect(app:wait("#captain-drive", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-drive", "2026-11-23-a.md")).to_be(true)
	app:command("file_report")
	expect(until_text(app, "#captain-drive", "2026-11-23-b.md")).to_be(true)
	-- (The tab it had, not a second one.)
	expect(kinds(app, "cap-a")).to_be("captain_drive")
end)
