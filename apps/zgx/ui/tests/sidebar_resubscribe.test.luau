-- SUR-248: the sidebar's row data (its derived status and its Activity
-- shelf/rank) follows the state it is made of. `screens/shell/sidebar.luau`'s
-- `session_of` read `chat.asks` and the Activity view's `on` with `:peek()`,
-- which is not a dependency in this runtime, so a row kept drawing what it
-- had until some `sessions.now` event made the list rebuild.
--
-- Both cases move one state only, and never publish a sessions event:
--   * `chat.asks` alone — the row's approval-vs-question refinement;
--   * `activity.on` alone — the shelf a chat stands on and its rank.
-- On the shell in ui/shell_fixture.luau.

local SIDE = "#shell-sidebar"
local ON = "screens/shell/activity.luau::shell_sidebar_activity"

local function open(): any
	local app = mount("shell_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	return app
end

-- What a row shows, its words and glyphs' names, as one line.
local function row_text(app: any, label: string): string
	return app:tree(`{SIDE} >> role:button "{label}"`)
end

-- Whether the sidebar shows a row starting with `label`.
local function shows(app: any, label: string): boolean
	return app:tree(SIDE):find(` button "{label}`, 1, true) ~= nil
end

test("SUR-248: chat.asks alone refines a row to its approval, with no sessions event", function()
	local app = open()
	-- (The chat page says what an agent asks; the session's own status does
	-- not move in this case — ui/shell_fixture.luau's `ask` command moves
	-- both, which is what hid the missing subscription.)
	expect(row_text(app, "Write the docs"):find("Pending Approval", 1, true)).to_be(nil)
	app:set_state("chat.asks", { a2 = "approval" })
	app:frames(30)
	expect(row_text(app, "Write the docs"):find("Pending Approval", 1, true) ~= nil).to_be(true)
end)

test("SUR-248: turning Activity on alone re-derives each row's shelf, with no sessions event", function()
	local app = mount("shell_fixture.luau")
	app:resize(1200, 900)
	-- (Cold: the list is built with the view off, so every row's shelf is
	-- nil and nothing is folded under a shelf's title.)
	app:set_state(ON, false)
	app:frames(30)
	expect(shows(app, "Working 1")).to_be(false)
	expect(shows(app, "Fix the build")).to_be(true)
	-- The view turns on; nothing about the sessions moves.
	app:set_state(ON, true)
	app:frames(40)
	expect(shows(app, "Working 1")).to_be(true)
	expect(shows(app, "Fix the build")).to_be(false)
end)
