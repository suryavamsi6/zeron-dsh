-- SUR-89: the sidebar lists the Captains in one quiet group above Projects
-- (never a second copy of a crew chat in its project), each Captain a row
-- with its crew count and its crew nested under it as ordinary chat rows;
-- a demoted (archived) Captain is not a row; the group folds from the
-- keyboard as the other groups do.
-- ui/captains_crew_fixture.luau stands the host's roster in; its
-- `refresh_captains` command makes the sidebar read it again after a test
-- sets the roster (the app re-reads it as a menu opens or an action lands).

local SIDE = "#shell-sidebar"

local function row(title: string): string
	return `{SIDE} >> role:button "{title}"`
end

local function shell(): any
	local app = mount("captains_crew_fixture.luau")
	app:resize(1200, 800)
	app:frames(30)
	return app
end

-- Gone from the tree, waited for as a fold closes a tick at a time.
local function until_gone(app: any, selector: string, ms: number?): boolean
	for _ = 1, math.ceil((ms or 15000) / 50) do
		if not app:has(selector) then
			return true
		end
		app:wait('"never shown, only waited past"', { timeout_ms = 50 })
	end
	return not app:has(selector)
end

-- The roster the fixture answers with, set after the mount and read again.
local function roster(app: any, captains: { [string]: string }, members: { [string]: string }, archived: { [string]: boolean }?)
	app:set_state("test_crew_captains", captains)
	app:set_state("test_crew_members", members)
	app:set_state("test_crew_archived", archived or {})
	app:command("refresh_captains")
	app:frames(12)
end

test("captains_sidebar: the group lists each Captain with its crew count and nests its crew chat, once", function()
	local app = shell()
	roster(app, { a1 = "Billing" }, { a2 = "a1" })
	expect(app:wait(row("Captains"), { timeout_ms = 15000 })).to_be(true)
	expect(app:wait(row("Billing"), { timeout_ms = 15000 })).to_be(true)
	-- The count is the row's own words.
	expect(app:tree(SIDE):find("1 crew", 1, true) ~= nil).to_be(true)
	-- The crew chat is listed once, under its Captain — not again in Projects.
	expect(app:wait(row("Write the docs"), { timeout_ms = 15000 })).to_be(true)
	expect(app:count(row("Write the docs"))).to_be(1)
	-- Its own Captain chat is still an ordinary chat row (only crews leave
	-- their project).
	expect(app:count(row("Fix the build"))).to_be(1)
end)

test("captains_sidebar: a Captain's row folds its crew away, and Enter brings it back", function()
	local app = shell()
	roster(app, { a1 = "Billing" }, { a2 = "a1" })
	expect(app:wait(row("Write the docs"), { timeout_ms = 15000 })).to_be(true)
	-- A press folds it (the row keeps the keyboard, so Enter opens it again).
	app:click(row("Billing"))
	expect(until_gone(app, row("Write the docs"))).to_be(true)
	app:key("Enter")
	app:frames(8)
	expect(app:wait(row("Write the docs"), { timeout_ms = 15000 })).to_be(true)
end)

test("captains_sidebar: a demoted Captain is not a row, and a roster with none has no group", function()
	local app = shell()
	roster(app, { a1 = "Billing" }, {}, { a1 = true })
	expect(app:wait(row("Fix the build"), { timeout_ms = 15000 })).to_be(true)
	expect(app:has(row("Billing"))).to_be(false)
	expect(app:has(row("Captains"))).to_be(false)
	-- (The chat it was is not hidden: it is a chat row in its project.)
	expect(app:count(row("Fix the build"))).to_be(1)
end)

test("captains_sidebar: a nested crew row opens its own chat", function()
	local app = shell()
	roster(app, { a1 = "Billing" }, { a2 = "a1" })
	expect(app:wait(row("Write the docs"), { timeout_ms = 15000 })).to_be(true)
	app:click(row("Write the docs"))
	expect(app:wait("#chat-composer", { timeout_ms = 15000 })).to_be(true)
end)
