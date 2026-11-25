-- SUR-91: a chat joins a Captain's crew from the chat's own menu, the
-- Captain's Digest beside it then has that crew's report, the roster offers
-- Remove from crew, and releasing it gives the chat its own page back. A
-- refused move says the engine's own sentence and leaves the row where it
-- was.
-- ui/captains_crew_fixture.luau stands the host's crew and Drive calls in.

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

local function pause(app: any, ms: number?)
	app:wait('"never shown, only waited past"', { timeout_ms = ms or 250 })
	app:frames(8)
end

local function shell(): any
	local app = mount("captains_crew_fixture.luau")
	app:resize(1200, 800)
	app:frames(30)
	return app
end

local function row(title: string): string
	return `#shell-sidebar >> role:button "{title}"`
end

-- The chat's menu, open (its roster read goes out as it opens).
local function menu(app: any, title: string)
	app:right_click(row(title))
	expect(app:wait('role:menuitem "Rename"', { timeout_ms = 15000 })).to_be(true)
	app:frames(4)
end

-- Move to Captain's submenu, open, and the Captain chosen in it.
local function move_to(app: any, captain: string)
	app:click('role:menuitem "Move to Captain"')
	expect(app:wait(`role:menuitem "{captain}"`, { timeout_ms = 15000 })).to_be(true)
	app:frames(4)
	app:click(`role:menuitem "{captain}"`)
end

local function sent(app: any): string
	return tostring(app:state("test_crew_calls") or "")
end

test("captains_crew: a chat moves into a Captain's crew from the Digest, the roster offers its release, and the released chat opens on its own page", function()
	local app = shell()
	app:set_state("test_crew_captains", { a1 = "Billing" })
	app:set_state("test_crew_members", {})
	app:set_state("test_crew_calls", "")
	-- The Captain's chat, its Digest opened: the page this is done from.
	app:click(row("Fix the build"))
	-- (The strip arrives with the session and lays out a frame later.)
	expect(app:wait('#captain-needs-you >> role:link "Open digest"', { timeout_ms = 15000 })).to_be(true)
	app:frames(10)
	app:click('#captain-needs-you >> role:link "Open digest"')
	expect(app:wait("#captain-digest", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-digest", "Two things are waiting on you.")).to_be(true)
	-- No crew yet, so no crew report.
	expect(app:tree("#captain-digest"):find("Write the docs", 1, true) ~= nil).to_be(false)

	-- Fix the build (a Captain) is never offered itself; Write the docs is
	-- offered it, in the same project.
	menu(app, "Write the docs")
	expect(app:has('role:menuitem "Remove from crew"')).to_be(false)
	app:click('role:menuitem "Move to Captain"')
	expect(app:wait('role:menuitem "Billing"', { timeout_ms = 15000 })).to_be(true)
	expect(app:has('role:menuitem "Fix the build"')).to_be(false)
	app:frames(4)
	app:click('role:menuitem "Billing"')
	pause(app)
	expect(sent(app)).to_be("add:a1:a2")

	-- The roster read back: the same chat's menu now releases it, and the
	-- words for joining and releasing are not the same words.
	menu(app, "Write the docs")
	expect(app:has('role:menuitem "Remove from crew"')).to_be(true)
	expect(app:has('role:menuitem "Move to Captain"')).to_be(false)
	app:key("escape")
	pause(app, 300)
	app:frames(10)

	-- The Captain's chat, read again as it is opened again (its Drive is
	-- read when the page comes, as the engine writes it when the crew files
	-- a report): the crew member's report is in the Digest.
	app:click(row("Tidy the tests"))
	app:frames(20)
	app:click(row("Fix the build"))
	expect(app:wait('#captain-needs-you >> role:link "Open digest"', { timeout_ms = 15000 })).to_be(true)
	app:frames(10)
	app:click('#captain-needs-you >> role:link "Open digest"')
	expect(app:wait("#captain-digest", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-digest", "Write the docs", 30000)).to_be(true)
	expect(until_text(app, "#captain-digest", "Write the docs finished its turn.")).to_be(true)

	-- Released: one send, and the roster puts it back among the chats that
	-- could join.
	menu(app, "Write the docs")
	app:click('role:menuitem "Remove from crew"')
	-- (The menu's scrim is still over the page as it closes.)
	pause(app, 400)
	app:frames(20)
	expect(sent(app)).to_be("add:a1:a2,remove:a1:a2")
	menu(app, "Write the docs")
	expect(app:has('role:menuitem "Move to Captain"')).to_be(true)
	expect(app:has('role:menuitem "Remove from crew"')).to_be(false)
	app:key("escape")
	pause(app, 300)
	app:frames(10)

	-- The released chat still opens on its own page: its own composer, and
	-- none of the Captain's strip (the chat it was released from keeps it).
	app:click(row("Write the docs"))
	app:frames(30)
	expect(app:wait("#chat-composer", { timeout_ms = 15000 })).to_be(true)
	-- Its own conversation, not the Captain's page it was in.
	expect(until_text(app, "#chat-session", "Write the docs", 30000)).to_be(true)
	app:click(row("Fix the build"))
	app:frames(30)
	expect(app:wait("#captain-needs-you", { timeout_ms = 15000 })).to_be(true)
end)

test("captains_crew: a refused move says the engine's rule, leaves the row where it was, and can be tried again", function()
	local app = shell()
	app:set_state("test_crew_captains", { a1 = "Billing" })
	app:set_state("test_crew_members", {})
	app:set_state("test_crew_calls", "")
	-- Another window assigned it first, or the project changed underneath:
	-- the roster this menu read is stale, and the engine's rule is what
	-- answers.
	app:set_state("test_crew_refuse", "a crew member must be in the Captain's project (a2 is in another project)")
	menu(app, "Write the docs")
	move_to(app, "Billing")
	pause(app, 400)
	expect(app:wait("role:alert", { timeout_ms = 15000 })).to_be(true)
	expect(app:tree("role:alert")).to_contain("a crew member must be in the Captain's project")
	-- Nothing changed locally: the row is still there, under its project,
	-- and its menu still offers the move.
	expect(app:has(row("Write the docs"))).to_be(true)
	menu(app, "Write the docs")
	expect(app:has('role:menuitem "Move to Captain"')).to_be(true)
	expect(app:has('role:menuitem "Remove from crew"')).to_be(false)
	-- And it can be tried again: this time the engine takes it.
	app:set_state("test_crew_refuse", "")
	move_to(app, "Billing")
	pause(app)
	expect(sent(app)).to_be("add:a1:a2,add:a1:a2")
	menu(app, "Write the docs")
	expect(app:has('role:menuitem "Remove from crew"')).to_be(true)
	app:key("escape")
	pause(app, 300)
	app:frames(10)
	-- The engine's other rule, in its own words: a chat is in at most one
	-- crew, when another window has put it in one this page has not seen.
	app:set_state("test_crew_refuse", "a chat is in at most one crew (a2 crews for b1)")
	app:set_state("test_crew_members", {})
	menu(app, "Write the docs")
	app:key("escape")
	pause(app, 300)
	menu(app, "Write the docs")
	expect(app:has('role:menuitem "Move to Captain"')).to_be(true)
	move_to(app, "Billing")
	pause(app)
	expect(app:wait("role:alert", { timeout_ms = 15000 })).to_be(true)
	expect(app:tree("role:alert")).to_contain("a chat is in at most one crew (a2 crews for b1)")
	app:set_state("test_crew_refuse", "")
end)

test("captains_crew: a Captain and a child chat say why they cannot move, instead of offering it", function()
	local app = shell()
	app:set_state("test_crew_captains", { a1 = "Billing" })
	app:set_state("test_crew_members", {})
	app:set_state("test_crew_calls", "")
	-- A Captain is already one.
	menu(app, "Fix the build")
	expect(app:has('role:menuitem "Move to Captain"')).to_be(true)
	expect(until_text(app, 'role:menu "Session actions"', "This chat is already a Captain")).to_be(true)
	app:key("escape")
	app:frames(6)
	-- A child chat has no delegation of its own to give (the panel's lists
	-- take this menu too).
	app:command("side_chats")
	app:frames(5)
	app:click('#shell-sidebar >> role:button "Fix the build"')
	app:frames(20)
	app:click('#titlebar >> role:button "Panel"')
	expect(app:wait("#panel-home-side-list", { timeout_ms = 15000 })).to_be(true)
	app:frames(30)
	app:right_click('#panel-home-side-list >> role:button "Open side chat Look into the flake"')
	expect(app:wait('role:menuitem "Rename"', { timeout_ms = 15000 })).to_be(true)
	app:frames(4)
	expect(app:has('role:menuitem "Move to Captain"')).to_be(true)
	expect(until_text(app, 'role:menu "Session actions"', "A child chat can't join a crew")).to_be(true)
	expect(sent(app)).to_be("")
end)

test("captains_crew: a demoted Captain's archived record is not offered, and its own chat is a chat again", function()
	local app = shell()
	-- The engine's roster keeps a demoted Captain as an archived row: Night
	-- shift (Tidy the tests) is one, beside the live Billing.
	app:set_state("test_crew_captains", { a1 = "Billing", a3 = "Night shift" })
	app:set_state("test_crew_archived", { a3 = true })
	-- Its crew is not a crew either: the archived record's members are
	-- nobody's crew, so their own row offers the move again.
	app:set_state("test_crew_members", { a2 = "a3" })
	-- (The roster is read as a menu opens, and the open that starts a read
	-- is built from the one before: this first open is the one behind.)
	menu(app, "Write the docs")
	app:key("escape")
	pause(app, 300)
	-- Write the docs was in the archived Captain's crew: not crewed, so its
	-- menu is a chat's, not a member's.
	menu(app, "Write the docs")
	expect(app:has('role:menuitem "Remove from crew"')).to_be(false)
	-- The live Captain is offered; the archived record is not.
	app:click('role:menuitem "Move to Captain"')
	expect(app:wait('role:menuitem "Billing"', { timeout_ms = 15000 })).to_be(true)
	expect(app:has('role:menuitem "Night shift"')).to_be(false)
	app:key("escape")
	pause(app, 300)
	app:frames(10)
	-- And the demoted chat itself is a chat: Promote, not Demote.
	menu(app, "Tidy the tests")
	expect(app:has('role:menuitem "Promote to Captain…"')).to_be(true)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(false)
	expect(sent(app)).to_be("")
end)
