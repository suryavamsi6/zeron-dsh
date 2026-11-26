-- A Captain's Digest (screens/chat/captain_digest): the needs-you strip over
-- its chat, the Digest tab it opens in the panel, and both reading the Drive
-- again as the Captain's turn writes it — in place, with no page reopened.
-- ui/captain_digest_fixture.luau stands the host's two answers in.

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

-- The session's panel tabs, in order ("captain_digest,files").
local function kinds(app: any, id: string): string
	local s = ((app:state("panel_sessions") :: any) or {})[`chat:{id}`]
	local out: { string } = {}
	for _, tab in (if s then s.tabs else {}) do
		table.insert(out, tab.kind)
	end
	return table.concat(out, ",")
end

test("the Digest tab opens from the strip and reads DIGEST.md with the crew's newest report", function()
	local app = mount("captain_digest_fixture.luau")
	home(app)
	-- The strip over the chat: what the Captain asks, and the way to its
	-- Digest.
	expect(app:wait("#captain-needs-you", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-needs-you", "Approve the invoice")).to_be(true)
	expect(until_text(app, "#captain-needs-you", "Pick a deploy window")).to_be(true)
	expect(until_text(app, "#captain-needs-you", "Needs you · 2")).to_be(true)
	-- Opened as a tab of the session's panel.
	app:click('#captain-needs-you >> role:link "Open digest"')
	expect(app:wait("#captain-digest", { timeout_ms = 15000 })).to_be(true)
	expect(kinds(app, "a1")).to_be("captain_digest")
	-- Its way again: the tab it has, not a second one.
	app:click('#captain-needs-you >> role:link "Open digest"')
	app:frames(10)
	expect(kinds(app, "a1")).to_be("captain_digest")
	-- DIGEST.md, through the message markdown's own renderer.
	expect(until_text(app, "#captain-digest", "Two things are waiting on you.")).to_be(true)
	expect(app:tree("#captain-digest")).to_contain("Billing")
	-- …and the crew's report under it, under the name it filed.
	expect(until_text(app, "#captain-digest", "Invoices reconciled; one needs your approval.")).to_be(true)
	expect(app:tree("#captain-digest")).to_contain("a2.md")
end)

test("the needs-you strip shows the five first asks only", function()
	local app = mount("captain_digest_fixture.luau")
	home(app)
	expect(app:wait("#captain-needs-you", { timeout_ms = 15000 })).to_be(true)
	app:command("many_asks")
	expect(until_text(app, "#captain-needs-you", "Needs you · 5")).to_be(true)
	expect(app:tree("#captain-needs-you")).to_contain("ask 5")
	expect(app:tree("#captain-needs-you")).never.to_contain("ask 6")
end)

test("a Drive that is not there yet hides the strip and leaves the Digest tab's empty state", function()
	local app = mount("captain_digest_fixture.luau")
	home(app)
	app:wait("#captain-needs-you", { timeout_ms = 15000 })
	-- (The tab, opened from its own way: with the strip empty there is no
	-- link to click, as a person with no asks never sees one.)
	app:command("open_digest")
	expect(app:wait("#captain-digest", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-digest", "Two things are waiting on you.")).to_be(true)
	app:command("clear_drive")
	expect(until_text(app, "#captain-digest", "No digest yet")).to_be(true)
	expect(until_text(app, "#captain-needs-you", "hidden")).to_be(true)
end)

test("a Captain's turn updates the strip and the open tab in place", function()
	local app = mount("captain_digest_fixture.luau")
	home(app)
	app:wait("#captain-needs-you", { timeout_ms = 15000 })
	app:click('#captain-needs-you >> role:link "Open digest"')
	expect(app:wait("#captain-digest", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-digest", "Invoices reconciled; one needs your approval.")).to_be(true)
	-- The turn ends: the Drive is written, then the reply lands.
	app:command("end_turn")
	expect(until_text(app, "#captain-needs-you", "Sign the renewal")).to_be(true)
	expect(until_text(app, "#captain-digest", "Renewal is signed and filed.")).to_be(true)
	-- The same tab, updated: nothing was opened again.
	expect(kinds(app, "a1")).to_be("captain_digest")
end)

test("demoting the Captain hides the strip and closes the open Digest tab", function()
	local app = mount("captain_digest_fixture.luau")
	home(app)
	expect(app:wait("#captain-needs-you", { timeout_ms = 15000 })).to_be(true)
	app:click('#captain-needs-you >> role:link "Open digest"')
	expect(app:wait("#captain-digest", { timeout_ms = 15000 })).to_be(true)
	expect(until_text(app, "#captain-digest", "Two things are waiting on you.")).to_be(true)
	app:command("demote_captain")
	expect(until_text(app, "#captain-needs-you", "hidden")).to_be(true)
	expect(kinds(app, "a1")).to_be("")
	expect(app:has("#captain-digest")).to_be(false)
end)
