-- SUR-246: a green Luau suite has to mean the window's screens loaded, not
-- only that the cases that ran passed.
--
-- What it cost to learn, measured on 2026-11-25: head `039a8265e` (SUR-89)
-- threw `screens/shell/sidebar.luau:538: attempt to index nil with 'get'` from
-- inside its `projects` computed while the window was built, 22 times in that
-- run's log — and `zgx test ui` answered `1539 passed, 0 failed`. I re-ran the
-- head here and got the same 1539/0. Two reasons, one per case below:
--
-- * nothing loaded the graph the app starts with, so a module that throws
--   inside `require` was only a line in the log (`ui/sur246_fixture.luau` now
--   loads it in the app's order and keeps the throw);
-- * the window already records a screen it could not build —
--   `screens/shell/boundary.luau` catches it, shows the error in that screen's
--   place and keeps the rest working, which is right — but no case read it, so
--   the sidebar could be an error box with every case green.

local function window(): any
	local app = mount("sur246_fixture.luau")
	app:resize(1200, 800)
	app:frames(30)
	return app
end

-- `app:error()` is the harness's own record of what the scripts threw while
-- mounting and drawing: 81 test files here assert it is nil, and it is the
-- reason a throw inside a computed is not invisible — it is only invisible to
-- a case that does not ask.

test("screen_graph: every screen the app starts with loads", function()
	local app = window()
	-- `sur246_fixture` requires the shell and the screens it builds inside a
	-- `pcall`, in `ui/app.luau`'s order, and writes what threw here.
	expect(tostring(app:state("test_sur246_load"))).to_be("")
	-- And nothing threw on the way: the graph loading is not the same as the
	-- window building without a runtime error.
	expect(app:error()).to_be_nil()
end)

test("screen_graph: the window records no screen it failed to build", function()
	local app = window()
	-- The window records this only once the shell has loaded.
	local ok, failures = pcall(function()
		return ((app :: any):state("shell_failures") :: any) or {}
	end)
	expect(ok).to_be(true)
	local said: { string } = {}
	for module, why in failures do
		table.insert(said, `{module} ({tostring(why)})`)
	end
	table.sort(said)
	-- Empty is the claim: an entry is a screen the person sees an error in.
	expect(table.concat(said, "; ")).to_be("")
	-- And the sidebar drew its rows, which is what its failed `projects`
	-- computed left empty on `039a8265e`.
	local order = ((app :: any):state("shell_sidebar_order") :: any) or {}
	expect(#order > 0).to_be(true)
	expect(app:error()).to_be_nil()
end)
