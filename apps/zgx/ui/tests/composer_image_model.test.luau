-- A picture carried on a model that can't read one (DeepSeek V4 Pro:
-- screens/chat/composer's NO_IMAGES, the sidecar's "This DeepSeek model
-- does not support image input"): the line above the composer says so
-- before the send would fail, and Send won't send while one is on —
-- until the picture is off, or the model is one that takes it. A file
-- is never one (on ui/chat_fixture.luau: s2 runs deepseek-v4-pro, s1
-- deepseek-flash).

local LINE = "DeepSeek V4 Pro can't read images. Switch to V4.1 Flash or remove the image."
local SEND = 'role:button "Send message"'
local ALERTS = 'role:button "Open Alerts"'
local BOX = 'role:textbox "Message"'

local function settle(app: any)
	app:frames(30)
end

local function chat(id: string): any
	local app = mount("chat_fixture.luau")
	app:resize(900, 700)
	app:command(`go_{id}`)
	settle(app)
	return app
end

-- Whether the line is up (the alert it raises opened to read it).
local function said(app: any): boolean
	if not app:has(ALERTS) then
		return false
	end
	app:click(ALERTS)
	settle(app)
	return app:has(`~"{LINE}"`)
end

test("V4 Pro carrying a picture says it can't read it; Send won't send it", function()
	local app = chat("s2")
	app:command("drop_pictures")
	settle(app)
	expect(said(app)).to_be(true)
	app:type(BOX, "look")
	settle(app)
	app:click(SEND)
	settle(app)
	-- Held back: nothing sent, the draft and its chips still there.
	expect(app:state("calls")).never.to_contain("send:s2:")
	expect(app:text(BOX)).to_contain("look")
	expect(app:has('role:button "zgx.png"')).to_be(true)
	expect(app:error()).to_be(nil)
end)

test("V4 Pro carrying only files says nothing, Send sends", function()
	local app = chat("s2")
	app:set_state("chat.composer.cargo", { { id = "p1", kind = "file", name = "spec.pdf", path = "README.md" } })
	settle(app)
	expect(app:has(ALERTS)).to_be(false)
	app:click(SEND)
	settle(app)
	expect(app:state("calls")).to_contain("send:s2:")
	expect(app:error()).to_be(nil)
end)

test("V4.1 Flash carrying a picture says nothing, Send sends", function()
	local app = chat("s1")
	app:command("drop_pictures")
	settle(app)
	expect(app:has(ALERTS)).to_be(false)
	app:click(SEND)
	settle(app)
	expect(app:state("calls")).to_contain("send:s1:")
	expect(app:error()).to_be(nil)
end)

test("the picture taken off clears it, Send back", function()
	local app = chat("s2")
	app:command("drop_pictures")
	settle(app)
	expect(said(app)).to_be(true)
	app:hover('role:button "zgx.png"')
	app:frames(10)
	app:click('role:button "Remove zgx.png"')
	settle(app)
	-- (The GIF and HEIC stay: files, no matter to it.)
	expect(app:has(ALERTS)).to_be(false)
	app:click(SEND)
	settle(app)
	expect(app:state("calls")).to_contain("send:s2:")
	expect(app:error()).to_be(nil)
end)
