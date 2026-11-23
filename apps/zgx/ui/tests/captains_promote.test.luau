-- SUR-88: a chat's own menu promotes it to a Captain and demotes it back; a
-- refused promotion says why in the engine's words, leaves the menu on
-- Promote and can be tried again; Cancel sends nothing.
-- ui/captains_promote_fixture.luau stands the host's three calls in.

local ROW = '#shell-sidebar >> role:button "Fix the build"'

local function pause(app: any, ms: number?)
	app:wait('"never shown, only waited past"', { timeout_ms = ms or 250 })
	app:frames(8)
end

local function shell(): any
	local app = mount("captains_promote_fixture.luau")
	app:resize(1200, 800)
	app:frames(30)
	return app
end

-- The chat's menu, open (its roster read goes out as it opens).
local function menu(app: any)
	app:right_click(ROW)
	expect(app:wait('role:menuitem "Rename"', { timeout_ms = 15000 })).to_be(true)
	app:frames(4)
end

local function sent(app: any): string
	return tostring(app:state("test_captain_calls") or "")
end

local function promote(app: any, name: string?)
	app:click('role:menuitem "Promote to Captain…"')
	expect(app:wait('role:textbox "Name"', { timeout_ms = 15000 })).to_be(true)
	-- (It arrives in, 200 ms: a click waits for it to be shown, not to exist.)
	pause(app, 300)
	-- Its Name begins as the chat's own title; a person's name replaces it.
	expect(app:text('role:textbox "Name"')).to_be("Fix the build")
	if name then
		app:fill('role:textbox "Name"', name)
	end
	app:click('role:button "Promote to Captain"')
end

test("captains_promote: the chat's menu promotes it, Demote replaces Promote, and Demote brings Promote back", function()
	local app = shell()
	menu(app)
	expect(app:has('role:menuitem "Promote to Captain…"')).to_be(true)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(false)
	promote(app, "Billing")
	pause(app)
	-- The role saved: the same chat's menu offers its way back.
	menu(app)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(true)
	expect(app:has('role:menuitem "Promote to Captain…"')).to_be(false)
	app:click('role:menuitem "Demote to chat"')
	-- (Asked first, saying where its crew goes.)
	expect(app:wait('role:button "Demote to chat"', { timeout_ms = 15000 })).to_be(true)
	pause(app, 300)
	expect(app:tree("role:dialog")).to_contain("Its crew goes back to your chat list.")
	app:click('role:button "Demote to chat"')
	pause(app)
	-- (The fixture answers a demote as the engine does: the chat stays on the
	-- roster as an archived record. Read as a Captain it would offer Demote
	-- again; the archived row is not one, so its menu is a chat's.)
	menu(app)
	expect(app:has('role:menuitem "Promote to Captain…"')).to_be(true)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(false)
	expect(sent(app)).to_be("promote:a1:Billing,demote:a1")
end)

test("captains_promote: a refused promotion says why, leaves the menu on Promote, and can be tried again", function()
	local app = shell()
	app:set_state("test_captains_refuse", true)
	menu(app)
	promote(app, "Billing")
	pause(app, 400)
	-- The engine's own words in a toast, with the way to the page that
	-- installs the plugin: not a dialog, nothing modal, the row as it was.
	expect(app:wait("role:alert", { timeout_ms = 15000 })).to_be(true)
	local toast = app:tree("role:alert")
	expect(toast).to_contain("bad params: a1 is not a chat this device knows")
	expect(toast).to_contain("Settings → DeepSeek plugins")
	menu(app)
	expect(app:has('role:menuitem "Promote to Captain…"')).to_be(true)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(false)
	-- Eligible to retry: the same menu, the same dialog, and this time it
	-- takes.
	app:set_state("test_captains_refuse", false)
	promote(app, "Billing")
	pause(app)
	menu(app)
	expect(app:has('role:menuitem "Demote to chat"')).to_be(true)
	expect(sent(app)).to_be("promote:a1:Billing,promote:a1:Billing")
end)

test("captains_promote: Cancel sends nothing and leaves the chat a chat", function()
	local app = shell()
	menu(app)
	app:click('role:menuitem "Promote to Captain…"')
	expect(app:wait('role:textbox "Name"', { timeout_ms = 15000 })).to_be(true)
	pause(app, 300)
	app:fill('role:textbox "Name"', "Billing")
	app:click('role:button "Cancel"')
	pause(app)
	expect(sent(app)).to_be("")
	menu(app)
	expect(app:has('role:menuitem "Promote to Captain…"')).to_be(true)
end)
