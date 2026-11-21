-- The composer's plan toggle in a DeepSeek chat, and the plan it sets: the
-- access panel's foot carries it (`ds/composer`'s `plan_row`), named "Plan
-- mode" for the person and for the screen's own tree; turning it on for a
-- chat writes it to the chat's config, and a new chat's first run carries it
-- as the run's own `plan`. On ui/chat_fixture.luau, whose one agent is
-- DeepSeek (`crates/harness/src/deepseek` supports plan mode) and whose
-- recorded calls (`calls`) name the run's plan `plan:true`.

local function open(go: string): any
	local app = mount("chat_fixture.luau")
	app:resize(1000, 800)
	app:command(go)
	app:frames(30)
	return app
end

local TOGGLE = 'role:switch "Plan mode"'

-- The toggle's row: its switch's checked state, read off the tree (`unchecked`
-- is how an off switch reads).
local function checked(app: any): boolean
	for line in app:tree():gmatch("[^\n]+") do
		if line:find('switch "Plan mode"', 1, true) then
			return not line:find("unchecked", 1, true)
		end
	end
	return false
end

test("the plan toggle stands in a DeepSeek chat's composer, named", function()
	local app = open("go_s2")
	app:click('role:button "Access"')
	app:frames(20)
	expect(app:has(TOGGLE)).to_be(true)
	-- Its name, in words, beside its hint.
	expect(app:tree()).to_contain("Plan mode")
	expect(app:tree()).to_contain("Explore and propose a plan before changing anything.")
end)

test("no plan toggle for an agent whose catalog entry reports no plan mode", function()
	local app = open("go_s2")
	-- The row is there for the agent it reports plan for...
	app:click('role:button "Access"')
	app:frames(20)
	expect(app:has(TOGGLE)).to_be(true)
	-- ...and goes when the same chat's agent reports it cannot enter plan
	-- mode: its catalog entry with `supportsPlan = false` (the host carries
	-- that field from ListHarnesses; an older engine leaves it out, read as
	-- false).
	local catalog = app:state("chat.catalog")
	local harness = catalog.harnesses[1]
	app:set_state("chat.catalog", {
		state = catalog.state,
		complete = catalog.complete,
		harnesses = { { id = harness.id, label = harness.label, mark = harness.mark, supportsPlan = false } },
		models = catalog.models,
	})
	app:frames(20)
	expect(app:has(TOGGLE)).to_be(false)
	expect(app:tree()).never.to_contain("Plan mode")
end)

test("turning plan mode on for a chat writes it to the chat's config", function()
	local app = open("go_s2")
	app:click('role:button "Access"')
	app:frames(20)
	app:click(TOGGLE)
	app:frames(20)
	expect(checked(app)).to_be(true)
	-- What the chat's next run is configured with (`chat.configure`).
	expect(app:state("calls")).to_contain("configure:s2:plan=true")
end)

test("a new chat's first run carries the plan the toggle set", function()
	local app = open("go_home")
	app:click('role:button "Access"')
	app:frames(20)
	app:click(TOGGLE)
	app:frames(20)
	expect(checked(app)).to_be(true)
	app:key("Escape")
	app:frames(10)
	app:type("role:textbox", "Plan the change")
	app:key("Enter")
	app:frames(30)
	local calls = app:state("calls")
	expect(calls).to_contain("send:new1:Plan the change")
	-- The run's config: its `plan`, as the host sends it.
	expect(calls).to_contain("plan:true")
end)
