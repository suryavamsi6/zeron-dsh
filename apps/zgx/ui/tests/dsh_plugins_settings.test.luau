-- Settings → DeepSeek plugins (screens/settings/dsh_plugins): the dsh
-- bundles installed for DeepSeek sessions, installing one from a spec, and
-- removing one after asking once. On the shell in ui/shell_fixture.luau,
-- `DshPlugins`, `DshPluginAdd` and `DshPluginRemove` answered by
-- ui/settings_stub.luau (`settings_stub_dsh_plugins`,
-- `settings_stub_dsh_plugin_add`, `settings_stub_dsh_plugin_remove`; the
-- ops asked for are in `settings_stub_dsh_plugin_calls`).

local PAGE = "#settings-page"
local NAV = "#settings-nav"
local REMOVE = `role:dialog "Remove “dsh-hello-plugin”?"`
local BUILD = `role:dialog "Allow “dsh-hello-plugin” to run its build script?"`

local function open(answer: any): any
	local app = mount("shell_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	if answer ~= nil then
		app:set_state("settings_stub_dsh_plugins", answer)
	end
	app:command("shell.settings")
	app:frames(10)
	app:click(`{NAV} >> "DeepSeek plugins"`)
	app:frames(30)
	return app
end

local function calls(app: any): { string }
	return (app:state("settings_stub_calls") :: any) or {}
end

local function asked(app: any, entry: string): number
	local n = 0
	for _, c in calls(app) do
		if c == entry then
			n += 1
		end
	end
	return n
end

test("DeepSeek plugins lists each bundle, its version, whether it's on, and the footer", function()
	local app = open()
	expect(app:has(`{PAGE} >> "dsh-hello-plugin"`)).to_be(true)
	expect(app:has(`{PAGE} >> "0.1.0"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Active"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Says hello"`)).to_be(true)
	expect(app:has(`{PAGE} >> "left-pad"`)).to_be(true)
	expect(app:has(`{PAGE} >> "1.3.0"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Not active"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Changes apply to new DeepSeek sessions."`)).to_be(true)
	expect(app:has(`{PAGE} >> "Installing needs pnpm 10 or newer on this device."`)).to_be(false)
	-- Asked once, as the page opened.
	expect(asked(app, "DshPlugins")).to_be(1)
end)

test("DeepSeek plugins with none installed is a quiet empty page", function()
	local app = open({ bundles = {}, pnpm = true })
	expect(app:has(`{PAGE} >> "No DeepSeek plugins installed"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Plugins are dsh bundles that add tools and skills to DeepSeek sessions."`)).to_be(true)
	-- (No bundle rows: "dsh-hello-plugin" would also match the field's
	-- placeholder, so its row's own words are what's absent.)
	expect(app:has(`{PAGE} >> "Says hello"`)).to_be(false)
	expect(app:has(`{PAGE} >> "0.1.0"`)).to_be(false)
end)

test("DeepSeek plugins says installing needs pnpm 10 when this device doesn't have it", function()
	local app = open({
		bundles = {
			{ name = "dsh-hello-plugin", spec = "^0.1.0", active = true, version = "0.1.0", description = "Says hello" },
		},
		pnpm = false,
	})
	expect(app:has(`{PAGE} >> "Installing needs pnpm 10 or newer on this device."`)).to_be(true)
	expect(app:has(`{PAGE} >> "dsh-hello-plugin"`)).to_be(true)
	expect(app:has(`{PAGE} >> "Active"`)).to_be(true)
end)

-- The `DshPluginAdd` ops the stub heard (`settings_stub_dsh_plugin_calls`).
local function added(app: any): { any }
	return (app:state("settings_stub_dsh_plugin_calls") :: any) or {}
end

-- The `DshPluginRemove` ops among them.
local function removed(app: any): { any }
	local out = {}
	for _, op in added(app) do
		if op.op == "remove" then
			table.insert(out, op)
		end
	end
	return out
end

test("DeepSeek plugins says on a bundle's row when it needs DSH Web", function()
	local app = open({
		bundles = {
			{ name = "dsh-hello-plugin", spec = "^0.1.0", active = true, version = "0.1.0", description = "Says hello", needsClient = true },
			{ name = "left-pad", spec = "^1.3.0", active = false, version = "1.3.0" },
		},
		pnpm = true,
	})
	expect(app:has(`{PAGE} >> "Part of this plugin needs DSH Web."`)).to_be(true)
	expect(app:count(`{PAGE} >> "Part of this plugin needs DSH Web."`)).to_be(1)
end)

test("DeepSeek plugins installs a typed spec on Install, then says so", function()
	local app = open()
	-- Empty, Install is off; the trust line is always under the field.
	expect(app:tree(`{PAGE} >> role:button "Install"`)).to_contain("disabled")
	expect(app:has(`{PAGE} >> "Installing runs the package's code on this device. Only install packages you trust."`)).to_be(true)
	app:fill(`{PAGE} >> role:textbox "Package"`, "dsh-hello-plugin")
	app:click(`{PAGE} >> role:button "Install"`)
	app:frames(10)
	local ops = added(app)
	expect(#ops).to_be(1)
	expect(ops[1].op).to_be("add")
	expect(ops[1].value).to_be("dsh-hello-plugin")
	-- Done: the field cleared, the note said, the list asked again.
	expect(app:text(`{PAGE} >> role:textbox "Package"`)).to_be("")
	expect(app:has(`{PAGE} >> "Installed. Applies to new sessions."`)).to_be(true)
	expect(asked(app, "DshPlugins")).to_be(2)
end)

test("DeepSeek plugins keeps Install disabled and says Installing while a run is on", function()
	local app = open()
	app:set_state("settings_stub_dsh_plugin_add", "pending")
	app:fill(`{PAGE} >> role:textbox "Package"`, "dsh-hello-plugin")
	app:click(`{PAGE} >> role:button "Install"`)
	app:frames(10)
	expect(#added(app)).to_be(1)
	expect(app:tree(`{PAGE} >> role:button "Install"`)).to_contain("disabled")
	expect(app:has(`{PAGE} >> "Installing…"`)).to_be(true)
	-- The typed spec kept while it runs.
	expect(app:text(`{PAGE} >> role:textbox "Package"`)).to_be("dsh-hello-plugin")
end)

test("DeepSeek plugins says why an install failed, from the refusal or the log's last six lines", function()
	local app = open()
	app:set_state("settings_stub_dsh_plugin_add", {
		ok = false,
		exitCode = 1,
		refusal = "This plugin is not compatible with the bundled dsh version (engines.dsh).",
		log = "line one\nline two",
	})
	app:fill(`{PAGE} >> role:textbox "Package"`, "dsh-hello-plugin")
	app:click(`{PAGE} >> role:button "Install"`)
	app:frames(10)
	expect(app:tree(PAGE)).to_contain("This plugin is not compatible with the bundled dsh version")
	-- The refusal said, not the log; the typed spec kept.
	expect(app:tree(PAGE)).never.to_contain("line two")
	expect(app:text(`{PAGE} >> role:textbox "Package"`)).to_be("dsh-hello-plugin")
	-- With no refusal, the run's last six log lines instead.
	app:set_state("settings_stub_dsh_plugin_add", {
		ok = false,
		exitCode = 1,
		log = "run-1\nrun-2\nrun-3\nrun-4\nrun-5\nrun-6\nrun-7\nrun-8",
	})
	app:click(`{PAGE} >> role:button "Install"`)
	app:frames(10)
	expect(app:tree(PAGE)).to_contain("run-8")
	expect(app:tree(PAGE)).to_contain("run-3")
	expect(app:tree(PAGE)).never.to_contain("run-2")
end)

test("DeepSeek plugins asks before a build script runs, Cancel making no call and Allow and install retrying with allowBuild", function()
	local app = open()
	app:set_state("settings_stub_dsh_plugin_add", {
		ok = false,
		exitCode = 1,
		needsBuildApproval = "dsh-hello-plugin",
		log = "Ignored build scripts: dsh-hello-plugin@0.1.0.",
	})
	app:fill(`{PAGE} >> role:textbox "Package"`, "dsh-hello-plugin")
	app:click(`{PAGE} >> role:button "Install"`)
	app:frames(30)
	expect(app:has(BUILD)).to_be(true)
	expect(app:has(`{BUILD} >> "A dependency of this plugin runs a build script at install time. Only allow it if you trust the source."`)).to_be(true)
	expect(app:has(`{BUILD} >> role:button "Cancel"`)).to_be(true)
	expect(app:has(`{BUILD} >> role:button "Allow and install"`)).to_be(true)
	-- One add asked, nothing allowed yet; Cancel asks nothing of the host.
	local ops = added(app)
	expect(#ops).to_be(1)
	expect(ops[1].allowBuild).to_be(nil)
	app:click(`{BUILD} >> role:button "Cancel"`)
	app:frames(30)
	expect(app:has(BUILD)).to_be(false)
	expect(#added(app)).to_be(1)
	-- Asked again, then allowed: the retry names it in allowBuild.
	app:click(`{PAGE} >> role:button "Install"`)
	app:frames(30)
	expect(app:has(BUILD)).to_be(true)
	app:set_state("settings_stub_dsh_plugin_add", { ok = true, exitCode = 0, log = "" })
	app:click(`{BUILD} >> role:button "Allow and install"`)
	app:frames(10)
	ops = added(app)
	expect(#ops).to_be(3)
	expect(ops[3].op).to_be("add")
	expect(ops[3].value).to_be("dsh-hello-plugin")
	expect(ops[3].allowBuild).to_be("dsh-hello-plugin")
	expect(app:has(`{PAGE} >> "Installed. Applies to new sessions."`)).to_be(true)
	expect(asked(app, "DshPlugins")).to_be(2)
end)

test("DeepSeek plugins asks once before removing a bundle, naming it", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove dsh-hello-plugin"`)
	app:frames(30)
	expect(app:has(REMOVE)).to_be(true)
	expect(app:has(`{REMOVE} >> "New DeepSeek sessions won't load it."`)).to_be(true)
	expect(app:has(`{REMOVE} >> role:button "Cancel"`)).to_be(true)
	expect(app:has(`{REMOVE} >> role:button "Remove plugin"`)).to_be(true)
	-- Asked about, not yet run.
	expect(#removed(app)).to_be(0)
end)

test("DeepSeek plugins cancels a remove on Escape, asking nothing of the host", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove dsh-hello-plugin"`)
	app:frames(30)
	expect(app:has(REMOVE)).to_be(true)
	app:key("Escape")
	app:frames(10)
	expect(#removed(app)).to_be(0)
	expect(asked(app, "DshPlugins")).to_be(1)
end)

test("DeepSeek plugins removes a bundle on Remove plugin and asks the list again", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Remove dsh-hello-plugin"`)
	app:frames(30)
	app:click(`{REMOVE} >> role:button "Remove plugin"`)
	app:frames(10)
	local ops = removed(app)
	expect(#ops).to_be(1)
	expect(ops[1].value).to_be("dsh-hello-plugin")
	expect(asked(app, "DshPlugins")).to_be(2)
end)

test("DeepSeek plugins keeps a bundle's Remove off while its remove runs", function()
	local app = open()
	app:set_state("settings_stub_dsh_plugin_remove", "pending")
	app:click(`{PAGE} >> role:button "Remove dsh-hello-plugin"`)
	app:frames(30)
	app:click(`{REMOVE} >> role:button "Remove plugin"`)
	app:frames(10)
	expect(#removed(app)).to_be(1)
	expect(app:tree(`{PAGE} >> role:button "Remove dsh-hello-plugin"`)).to_contain("disabled")
	-- The other bundle's is still on.
	expect(app:tree(`{PAGE} >> role:button "Remove left-pad"`)).never.to_contain("disabled")
end)

test("DeepSeek plugins says why a remove failed, under the bundle's row", function()
	local app = open()
	app:set_state("settings_stub_dsh_plugin_remove", {
		ok = false,
		exitCode = 1,
		refusal = "This plugin is in use by a running session.",
		log = "line one\nline two",
	})
	app:click(`{PAGE} >> role:button "Remove dsh-hello-plugin"`)
	app:frames(30)
	app:click(`{REMOVE} >> role:button "Remove plugin"`)
	app:frames(10)
	-- The refusal said under the row, not the log; the list not asked again.
	expect(app:tree(PAGE)).to_contain("This plugin is in use by a running session.")
	expect(app:tree(PAGE)).never.to_contain("line two")
	expect(asked(app, "DshPlugins")).to_be(1)
	-- With no refusal, the run's last six log lines instead.
	app:set_state("settings_stub_dsh_plugin_remove", {
		ok = false,
		exitCode = 1,
		log = "run-1\nrun-2\nrun-3\nrun-4\nrun-5\nrun-6\nrun-7\nrun-8",
	})
	app:click(`{PAGE} >> role:button "Remove dsh-hello-plugin"`)
	app:frames(30)
	app:click(`{REMOVE} >> role:button "Remove plugin"`)
	app:frames(10)
	expect(app:tree(PAGE)).to_contain("run-8")
	expect(app:tree(PAGE)).to_contain("run-3")
	expect(app:tree(PAGE)).never.to_contain("run-2")
end)
