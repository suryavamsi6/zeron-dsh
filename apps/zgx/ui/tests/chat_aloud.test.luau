-- "Read aloud" (screens/chat/aloud): a DeepSeek reply's action sends its
-- words to the host (`ReadAloud`, stubbed in ui/chat_fixture.luau), its
-- button the read's Stop until the call — which lasts the read — ends.

local function settle(app: any)
	app:frames(30)
end

local function open(app: any, id: string)
	app:command(`go_{id}`)
	settle(app)
end

test("a DeepSeek reply offers Read aloud; it calls the host, and is Stop until the read ends", function()
	local app = mount("chat_fixture.luau")
	app:resize(900, 700)
	open(app, "s1")
	app:command("session_on_deepseek")
	settle(app)
	app:hover('~"Part 30 works like this"')
	settle(app)
	expect(app:has('role:button "Copy message"')).to_be(true)
	app:click('role:button "Read aloud"')
	settle(app)
	-- The reply's words (what its Copy copies) went to the host.
	expect(app:state("calls")).to_contain("read_aloud:Part 30 works like this.")
	-- While the read goes the button is its stop.
	expect(app:has('role:button "Stop reading"')).to_be(true)
	expect(app:has('role:button "Read aloud"')).to_be(false)
	app:click('role:button "Stop reading"')
	settle(app)
	expect(app:state("calls")).to_contain("stop_read_aloud")
	-- Ended, it's Read aloud again; the sound itself ending does the same.
	expect(app:has('role:button "Read aloud"')).to_be(true)
	app:click('role:button "Read aloud"')
	settle(app)
	expect(app:has('role:button "Stop reading"')).to_be(true)
	app:command("aloud_ends")
	settle(app)
	expect(app:has('role:button "Read aloud"')).to_be(true)
end)

test("a prompt's strip offers none, nor does a reply's off DeepSeek", function()
	local app = mount("chat_fixture.luau")
	app:resize(900, 700)
	open(app, "s6")
	-- The host hasn't put this chat on DeepSeek: a reply's only Copy.
	app:hover('~"Paint is second"')
	settle(app)
	expect(app:has('role:button "Copy message"')).to_be(true)
	expect(app:has('role:button "Read aloud"')).to_be(false)
	-- On DeepSeek now: a person's own message still offers none.
	app:command("session_on_deepseek")
	settle(app)
	app:hover('~"Carry on from here"')
	settle(app)
	expect(app:has('role:button "Copy message"')).to_be(true)
	expect(app:has('role:button "Read aloud"')).to_be(false)
	-- Its reply does.
	app:hover('~"Here is the summary"')
	settle(app)
	expect(app:has('role:button "Read aloud"')).to_be(true)
end)
