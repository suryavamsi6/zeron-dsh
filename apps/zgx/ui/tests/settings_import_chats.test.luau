-- Settings → Import chats: a DeepSeek v3 export ZIP picked from this Mac
-- goes to the engine as `ImportDeepSeekChats { path }`, once per pick and
-- never when the chooser is cancelled; the counts shown are the engine's
-- own (`imported`, `alreadyPresent`, `kept`, `total`), a refusal is said in
-- its words with Try again sending the same path once more. Then the
-- imported chats open read-only: the reason stands, Send is disabled, and
-- an ordinary chat still composes beside them. On the shell in
-- ui/shell_fixture.luau (signed in as Ada, the engine answered by
-- ui/settings_stub.luau) and the chat page in ui/chat_fixture.luau.

local PAGE = "#settings-page"
local NAV = "#settings-nav"

local function settings(): any
	local app = mount("shell_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	app:command("settings_stub.sign_in")
	app:command("shell.settings")
	app:frames(20)
	-- (Last in the list, past the foot: scrolled to the end first.)
	app:scroll(NAV, 2000)
	app:frames(10)
	app:click(`{NAV} >> role:tab "Import chats"`)
	app:frames(30)
	return app
end

-- The paths the engine was asked to import, in order.
local function imports(app: any): { string }
	return (app:state("settings_stub_import_calls") :: any) or {}
end

local function chat(): any
	local app = mount("chat_fixture.luau")
	app:resize(1000, 800)
	app:frames(30)
	return app
end

test("Import chats asks the engine nothing when the chooser is cancelled", function()
	local app = settings()
	app:set_state("settings_stub_files", {})
	app:click("#settings-import-chats")
	app:frames(20)
	expect(#imports(app)).to_be(0)
	-- (The chooser itself was opened; no import followed it.)
	local calls = table.concat((app:state("settings_stub_calls") :: any) or {}, ",")
	expect(calls).to_contain("open_files")
	expect(calls).never.to_contain("ImportDeepSeekChats")
	expect(app:has(`{PAGE} >> "Choose file…"`)).to_be(true)
end)

test("Import chats sends the picked path once and shows the engine's counts", function()
	local app = settings()
	app:set_state("settings_stub_files", { { path = "/tmp/deepseek-export.zip", name = "deepseek-export.zip" } })
	app:set_state("settings_stub_import_answer", {
		imported = 3,
		alreadyPresent = 1,
		chats = { "deepseek-1", "deepseek-2", "deepseek-3", "deepseek-4" },
		total = 5,
		kept = 4,
	})
	app:click("#settings-import-chats")
	app:frames(20)
	expect(#imports(app)).to_be(1)
	expect(imports(app)[1]).to_be("/tmp/deepseek-export.zip")
	-- As the engine answered: counts and the cap it reported.
	expect(app:has(`{PAGE} >> "3 chats imported."`)).to_be(true)
	expect(app:has(`{PAGE} >> "1 was already here."`)).to_be(true)
	expect(app:has(`{PAGE} >> "4 of 5 conversations kept."`)).to_be(true)
end)

test("Import chats says the newest kept when the engine reports no total", function()
	local app = settings()
	app:set_state("settings_stub_files", { { path = "/tmp/deepseek-export.zip", name = "deepseek-export.zip" } })
	-- (The engine's `total` is null past its ceiling: it kept the newest 1000
	-- and invented no count, so the page must say that rather than hiding
	-- that anything was left out.)
	app:set_state("settings_stub_import_answer", {
		imported = 1000,
		alreadyPresent = 0,
		chats = {},
		kept = 1000,
	})
	app:click("#settings-import-chats")
	app:frames(20)
	expect(app:has(`{PAGE} >> "1000 chats imported."`)).to_be(true)
	expect(app:has(`{PAGE} >> "The export holds 10,000 or more conversations; the newest 1000 were kept."`)).to_be(true)
	-- (No "kept of total" row: there is no total to say.)
	expect(app:has(`{PAGE} >> "1000 of 0 conversations kept."`)).to_be(false)
end)

test("Import chats says the engine's refusal and retries the same path", function()
	local app = settings()
	app:set_state("settings_stub_files", { { path = "/tmp/deepseek-export.zip", name = "deepseek-export.zip" } })
	app:set_state("settings_stub_refuse", true)
	app:click("#settings-import-chats")
	app:frames(20)
	expect(#imports(app)).to_be(1)
	expect(app:has(`{PAGE} >> "The export could not be read."`)).to_be(true)
	app:click("#settings-import-retry")
	app:frames(20)
	expect(#imports(app)).to_be(2)
	expect(imports(app)[2]).to_be("/tmp/deepseek-export.zip")
	-- (The default answer, 2 imported: the retry went through.)
	expect(app:has(`{PAGE} >> "2 chats imported."`)).to_be(true)
	expect(app:has(`{PAGE} >> "The export could not be read."`)).to_be(false)
end)

test("an imported chat opens read-only: the reason stands and Send is disabled", function()
	local app = chat()
	app:command("open_imported")
	app:frames(30)
	expect(app:has('"This chat was imported from DeepSeek and is read-only."')).to_be(true)
	expect(app:has(`"Plan the migration"`)).to_be(true)
	-- No Send: its button is there but held, and nothing typed goes.
	expect(app:tree('role:button "Send message"')).to_contain("disabled")
	app:type("role:textbox", "not going anywhere")
	app:key("Enter")
	app:frames(20)
	expect(app:state("calls")).never.to_contain("send:")
end)

test("an ordinary chat still composes beside an imported one", function()
	local app = chat()
	app:command("go_s2")
	app:frames(30)
	app:type("role:textbox", "Ship it")
	app:key("Enter")
	app:frames(30)
	expect(app:state("calls")).to_contain("send:s2:Ship it")
end)
