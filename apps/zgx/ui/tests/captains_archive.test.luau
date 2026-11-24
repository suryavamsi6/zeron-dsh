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

-- A condition waited for up to a ceiling, with how long it waited: the
-- refusal's toast is shown after the roster re-read that follows the call,
-- so its tick is the host's, not the page's. A fixed pause cannot tell "the
-- page never drew it" from "the runner was slower than the pause" — this
-- case's 600 ms passed on this host and failed on CI's loaded one. (The
-- harness advances a frame clock, so the wait is counted in frames, the
-- page's own unit, rather than in wall seconds no test can read.)
local function until_true(app: any, wanted: () -> boolean, ceiling: number): (boolean, string)
	local frames = 0
	while true do
		if wanted() then
			return true, `{frames} frames`
		end
		if frames >= ceiling then
			return false, `{frames} frames (the ceiling)`
		end
		app:frames(2)
		frames += 2
	end
end

-- Every toast standing, as the state the drawn one is built from: the case
-- then does not rest on catching one frame of a live toast.
local function toasts(app: any): string
	local out = {}
	for _, t in app:state("ds/toast.luau::ds_toasts") or {} do
		table.insert(out, `{tostring(t.title)} {tostring(t.body)}`)
	end
	return table.concat(out, "\n")
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
	-- went to the shelf with it. The Captain's own chat keeps its row — the
	-- host archives the crew's chats and not its own, so nothing is hidden
	-- that the person did not hide.)
	expect(shows(app, "Billing")).to_be(false)
	expect(shows(app, "Write the docs")).to_be(false)
	expect(shows(app, "Fix the build")).to_be(true)
end)

test("SUR-251: a put-away Captain's own row offers Unarchive and Delete, and wakes it", function()
	local app = shell()
	menu(app)
	app:click('role:menuitem "Archive Captain" [1]')
	pause(app)
	-- (Its chat keeps an ordinary row, so the asleep menu is reached on that
	-- row — not on the shelf, which carries the crew's chats. This is the
	-- surface Chief's clause names: one archive item, one delete item, and
	-- the chat's own Archive and Delete absent.)
	menu(app)
	expect(app:has('role:menuitem "Unarchive Captain"')).to_be(true)
	expect(app:has('role:menuitem "Delete Captain"')).to_be(true)
	expect(app:has('role:menuitem "Archive Captain"')).to_be(false)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(false)
	expect(app:has('role:menuitem "Archive"')).to_be(false)
	expect(app:has('role:menuitem "Delete…"')).to_be(false)
	app:click('role:menuitem "Unarchive Captain"')
	pause(app, 400)
	-- (The Captain's own verb, not the chat's: it also brings the crew back.)
	expect(sent(app)).to_be("archive:a1,unarchive:a1")
	expect(shows(app, "Billing")).to_be(true)
	expect(shows(app, "Write the docs")).to_be(true)
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
	local why = "The Captain isn't in the registry"
	app:set_state("test_sur251_refuse", why)
	menu(app)
	app:click('role:menuitem "Delete Captain"')
	expect(app:wait('role:button "Delete"', { timeout_ms = 15000 })).to_be(true)
	pause(app, 300)
	app:click('role:button "Delete"')
	-- (The toast follows the roster re-read, so it is waited for rather than
	-- sampled at a fixed tick — and the wait says how long it took.)
	local drawn, waited = until_true(app, function(): boolean
		return app:has(`"{why}"`)
	end, 480)
	expect(`the refusal's toast after {waited}: {drawn}`).to_be(`the refusal's toast after {waited}: true`)
	expect(sent(app)).to_be("delete:a1")
	expect(shows(app, "Billing")).to_be(true)
	expect(app:tree()).to_contain(why)
	-- (And in the state the toast is built from, not only on screen.)
	expect(toasts(app):find(why, 1, true) ~= nil).to_be(true)
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
