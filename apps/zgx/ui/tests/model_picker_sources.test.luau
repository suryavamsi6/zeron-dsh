-- A model's row says its name, and no provider: the one runtime names none
-- (a model's provider was for an agent reaching models through several, and
-- there is one now). A search finds a model by its name; a provider's word
-- finds nothing. On the picker in ui/picker_sources_fixture.luau.

local function settle(app: any)
	app:frames(30)
end

-- Opened out on the chosen model.
local function open(app: any)
	app:click('role:button "Model"')
	settle(app)
	app:click(".folded")
	settle(app)
end

local LONG = "DeepSeek V4 Pro Max Preview Extended Context"

test("every row says its model's name, and no provider", function()
	local app = mount("picker_sources_fixture.luau")
	app:resize(600, 700)
	settle(app)
	open(app)
	-- The rows: their names, whole, and nothing beside them.
	expect(app:has('role:option "DeepSeek V4.1 Flash"')).to_be(true)
	expect(app:has(`role:option "{LONG}"`)).to_be(true)
	-- A search finds a model by its name; a provider's word finds nothing.
	app:fill('role:textbox "Search"', "flash")
	settle(app)
	expect(app:has('role:option "DeepSeek V4.1 Flash"')).to_be(true)
	expect(app:has(`role:option "{LONG}"`)).to_be(false)
	app:fill('role:textbox "Search"', "codex")
	settle(app)
	expect(app:count("role:option")).to_be(0)
	expect(app:error()).to_be(nil)
end)
