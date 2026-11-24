-- SUR-251: a Captain is put away, woken and deleted from the sidebar. Archive
-- sends CaptainArchive and the Captain leaves the live group (its crew goes
-- with it, on the shelf); the shelf's Unarchive sends CaptainUnarchive, which
-- is a different call from the chat's own unarchive; Delete asks first with
-- the issue's sentence and keeps the crew; a refusal leaves the row.
-- ui/sur251_fixture.luau stands the host's three actions in.

local SIDE = "#shell-sidebar"
-- (The Captain's own chat row, where Demote already lives: the Captain row
-- itself is a fold, with no menu of its own.)
local ROW = `{SIDE} >> role:button "Fix the build"`

local function pause(app: any, ms: number?)
	app:wait('"never shown, only waited past"', { timeout_ms = ms or 250 })
	app:frames(8)
end

local function shell(): any
	local app = mount("sur251_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	return app
end

-- The chat's menu, open (its roster read goes out as it opens).
local function menu(app: any)
	app:right_click(ROW)
	expect(app:wait('role:menuitem "Rename"', { timeout_ms = 15000 })).to_be(true)
	app:frames(6)
end

local function sent(app: any): string
	return tostring(app:state("test_sur251_calls") or "")
end

local function shows(app: any, label: string): boolean
	return app:tree(SIDE):find(` button "{label}`, 1, true) ~= nil
end

test("SUR-251: a live Captain's menu offers Archive and Delete beside Demote", function()
	local app = shell()
	menu(app)
	expect(app:has('role:menuitem "Archive Captain"')).to_be(true)
	expect(app:has('role:menuitem "Delete Captain"')).to_be(true)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(true)
	expect(app:has('role:menuitem "Unarchive Captain"')).to_be(false)
end)

test("SUR-251: Archive sends CaptainArchive and the Captain leaves the live group with its crew", function()
	local app = shell()
	expect(shows(app, "Billing")).to_be(true)
	menu(app)
	app:click('role:menuitem "Archive Captain" [1]')
	pause(app)
	expect(sent(app)).to_be("archive:a1")
	-- (Off the live group: the roster re-read says it is asleep, and its crew
	-- went to the shelf with it.)
	expect(shows(app, "Billing")).to_be(false)
	expect(shows(app, "Write the docs")).to_be(false)
	expect(shows(app, "Fix the build")).to_be(false)
end)

test("SUR-251: the shelf wakes it with CaptainUnarchive, not the chat's own unarchive", function()
	local app = shell()
	menu(app)
	app:click('role:menuitem "Archive Captain" [1]')
	pause(app)
	-- (Its own chat is on the Archived shelf, as the host archived it.)
	app:set_state("screens/shell/sidebar.luau::shell_sidebar_show_archived", true)
	app:frames(30)
	expect(app:wait(`{SIDE} >> role:button "Fix the build"`, { timeout_ms = 15000 })).to_be(true)
	app:hover(`{SIDE} >> role:button "Fix the build"`)
	pause(app)
	-- (The shelf's corner is the chat rows' own, one label for every row:
	-- only the row under the pointer shows it, so it is not ambiguous here.)
	app:click(`{SIDE} >> role:button "Unarchive"`)
	pause(app, 400)
	expect(sent(app)).to_be("archive:a1,unarchive:a1")
	-- (Back in the live group, and not twice on the shelf.)
	expect(shows(app, "Billing")).to_be(true)
end)

test("SUR-251: Delete asks first in the issue's words, then removes the row and keeps the crew", function()
	local app = shell()
	menu(app)
	app:click('role:menuitem "Delete Captain"')
	expect(app:wait('role:button "Delete"', { timeout_ms = 15000 })).to_be(true)
	pause(app, 300)
	expect(app:tree("role:dialog")).to_contain("Delete Captain Billing? Its crew stays in your chat list.")
	app:click('role:button "Delete"')
	pause(app, 400)
	expect(sent(app)).to_be("delete:a1")
	expect(shows(app, "Billing")).to_be(false)
	-- (The crew is not deleted with it: its chat stays in the list.)
	expect(shows(app, "Write the docs")).to_be(true)
end)

test("SUR-251: a refused delete leaves the row and says why", function()
	local app = shell()
	app:set_state("test_sur251_refuse", "The Captain isn't in the registry")
	menu(app)
	app:click('role:menuitem "Delete Captain"')
	expect(app:wait('role:button "Delete"', { timeout_ms = 15000 })).to_be(true)
	pause(app, 300)
	app:click('role:button "Delete"')
	pause(app, 600)
	expect(sent(app)).to_be("delete:a1")
	expect(shows(app, "Billing")).to_be(true)
	expect(app:tree()).to_contain("The Captain isn't in the registry")
end)

test("SUR-251: a Captain's row has one archive and one delete, and the delete is the Captain's", function()
	local app = shell()
	menu(app)
	-- (The chat's own Archive and Delete… are gone from a Captain's row:
	-- archiving or deleting the chat under a live record is the same
	-- inconsistency, whichever verb does it.)
	expect(app:has('role:menuitem "Archive"')).to_be(false)
	expect(app:has('role:menuitem "Delete…"')).to_be(false)
	expect(app:count('role:menuitem "Archive Captain"')).to_be(1)
	expect(app:count('role:menuitem "Delete Captain"')).to_be(1)
	-- (And the one delete offered is the registry's, not the session's.)
	app:click('role:menuitem "Delete Captain"')
	expect(app:wait('role:button "Delete"', { timeout_ms = 15000 })).to_be(true)
	pause(app, 300)
	app:click('role:button "Delete"')
	pause(app, 400)
	expect(sent(app)).to_be("delete:a1")
end)
