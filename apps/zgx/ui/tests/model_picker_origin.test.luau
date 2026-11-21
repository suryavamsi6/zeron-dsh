-- The picker says where its list came from. A row the engine could only
-- fill from the shipped catalog carries the engine's `description` as its
-- hint, in place of the provider name; an account-sourced row keeps the
-- provider name it has always had; a model with neither says nothing. On
-- the picker in ui/picker_origin_fixture.luau.

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

test("a row the engine took from the shipped catalog says so", function()
	local app = mount("picker_origin_fixture.luau")
	app:resize(600, 700)
	settle(app)
	open(app)
	-- The description, not the provider: where the list came from.
	expect(app:has('role:option ~"Shipped catalog — 401"')).to_be(true)
	-- On its own row alone, and the fallback row's provider name is gone
	-- with it: OpenRouter survives only on the account-sourced row.
	expect(app:count('role:option ~"Shipped catalog — 401"')).to_be(1)
	expect(app:count('role:option ~"OpenRouter"')).to_be(1)
	expect(app:error()).to_be(nil)
end)

test("an account-sourced row keeps its provider's name", function()
	local app = mount("picker_origin_fixture.luau")
	app:resize(600, 700)
	settle(app)
	open(app)
	-- No description: the row says where the model is served from, as before.
	expect(app:has('role:option ~"OpenRouter"')).to_be(true)
end)

test("a model with no provider and no description says only its name", function()
	local app = mount("picker_origin_fixture.luau")
	app:resize(600, 700)
	settle(app)
	open(app)
	-- The runtime's own row, unchanged: nothing beside the name.
	expect(app:has('role:option "DeepSeek V4.1 Flash"')).to_be(true)
end)
