-- Settings → DeepSeek plugins (screens/settings/dsh_plugins): the dsh
-- bundles installed for DeepSeek sessions, read-only. On the shell in
-- ui/shell_fixture.luau, `DshPlugins` answered by ui/settings_stub.luau
-- (`settings_stub_dsh_plugins` overrides it).

local PAGE = "#settings-page"
local NAV = "#settings-nav"

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
	expect(app:has(`{PAGE} >> "dsh-hello-plugin"`)).to_be(false)
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
