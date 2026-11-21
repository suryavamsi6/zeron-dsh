-- A plan-review question is the SDK's `user-question/ask` with intent
-- `plan-review`: the plan itself rides in `question.detail`, which the host
-- puts on the question record as `detail` (SUR-166). The card draws that body
-- under the question's text, as the approval card draws its own (SUR-167) —
-- without it a person in plan mode was asked to approve a plan they could
-- not see. A question carrying none draws nothing extra.

local function settle(app: any)
	app:frames(30)
end

local function open(app: any, id: string)
	app:command(`go_{id}`)
	settle(app)
end

test("a question carrying a plan body draws it under the question's text", function()
	local app = mount("chat_fixture.luau")
	app:resize(900, 700)
	open(app, "s2")
	app:command("ask_question_plan")
	settle(app)
	-- (The head is drawn in caps, as every ask's kind is.)
	expect(app:has('~"APPROVE PLAN"')).to_be(true)
	expect(app:has('~"Approve this plan to leave plan mode?"')).to_be(true)
	-- The plan's own words: the reason the question is asked.
	expect(app:has('~"Pin the inner width"')).to_be(true)
	-- Under the question, not above it or in a tooltip.
	local tree = app:tree("#chat-composer")
	local asked = tree:find("Approve this plan to leave plan mode?", 1, true)
	local plan = tree:find("Pin the inner width", 1, true)
	expect(asked ~= nil and plan ~= nil and asked < plan).to_be(true)
	-- Its options are still the card's: the body does not replace them.
	local options = app:tree("#chat-composer")
	expect(options).to_contain('radio "Approve"')
	expect(options).to_contain('radio "Decline"')
end)

test("a question with no plan body draws none", function()
	local app = mount("chat_fixture.luau")
	app:resize(900, 700)
	open(app, "s2")
	app:command("ask_question")
	settle(app)
	-- The card itself is there, so the absence below is the body's absence.
	expect(app:has('~"Which branch should the fix go on?"')).to_be(true)
	expect(app:tree("#chat-composer")).never.to_contain("Pin the inner width")
end)
