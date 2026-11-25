-- SUR-246: the screen graph the app starts with, loaded and reported, then the
-- window itself.
--
-- `ui/app.luau` builds the main window from these modules; the shell pulls in
-- every screen of that window (the sidebar, the title bar, the page, the chat
-- page, the panel) through `screens/shell/boundary.luau`, which catches a
-- module that cannot load so the rest of the window keeps working — right, and
-- the reason a throw can go unseen: the window shows the error in the failed
-- screen's place and every case that does not read that screen still passes.
--
-- Measured on head `039a8265e` (SUR-89) on 2026-11-25: `screens/shell/sidebar`
-- threw `attempt to index nil with 'get'` from inside its `projects` computed
-- while the window was being built, 22 times in the CI log, and `zgx test ui`
-- printed `1539 passed, 0 failed` — I re-ran that head here and got the same
-- 1539/0, with the sidebar installed again fine on the next read.
--
-- So this loads the graph in the app's own order and keeps a throw, and the
-- case reads both that and the window's own `shell_failures`.

local load_failures = ui.state("test_sur246_load", "" :: string)

local function loads(path: string): boolean
	local ok, err = pcall(require, path)
	if not ok then
		load_failures:set(`{load_failures:peek()}{path}: {tostring(err)}; `)
	end
	return ok
end

-- (What `ui/app.luau` does for the main window, in its order: the design
-- system, then the language, the plugins page and the shell.)
local ds = require("@ds")
ds.setup()
-- (The host stand-ins first, as `shell_fixture` requires them at its `:135`
-- before it requires the shell: the settings screens ask `settings.engine` as
-- they load and the appearance module applies the scheme through the host, and
-- unanswered those are runtime errors: the harness records them in
-- `app:error()`, prints them, and passes every case that does not ask — the
-- silence this check exists to break. Measured: without these two lines the
-- fixture threw six such errors on every mount and both cases stayed green.)
require("./settings_stub")
require("@screens/onboarding/actions").apply_scheme = function() end

local loaded = true
for _, path in { "@app/screens/language", "@app/screens/plugins", "@app/screens/shell" } do
	-- (`loads` keeps the throw; the loop keeps every module attempted, so one
	-- failure does not hide the others.)
	loaded = loads(path) and loaded
end

-- A graph that did not load still mounts, so the case asserts the capture
-- rather than dying in the harness's own mount error — with a throw inside
-- `require("./shell_fixture")` the assertion is never reached.
if not loaded then
	return ui.text { "the app's start graph did not load", literal = true }
end

-- The window, with the chats and the host stubs the shell needs.
return require("./shell_fixture")
