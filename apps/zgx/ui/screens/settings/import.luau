-- Settings → Import chats: a DeepSeek v3 export ZIP on this Mac, imported by
-- the engine (`ImportDeepSeekChats`, crates/engine/src/deepseek_import.rs)
-- as ordinary chat rows plus their transcripts — read-only, beside the
-- person's own. The platform's file chooser picks the ZIP; cancelling asks
-- the engine nothing. The answer is the engine's own counts (`imported`,
-- `alreadyPresent`, `kept`, `total`) and is shown as it came, never worked
-- out here; a refusal (bad file, a key that could not read it) is said in
-- the engine's words with Try again sending the same path once more.
--
-- The imported rows arrive in the sidebar through the engine's chat watch
-- (src/sessions.rs `WatchChats`), not by this page asking again.

local ds = require("@ds")
local host = require("@app/screens/settings/host")
local search = require("@app/screens/settings/search")

local M = { ID = "import", TITLE = "Import chats" }
search.index(M.ID, "Import chats", "import deepseek chats export zip read only conversations", {
	{
		key = "import",
		title = "Import DeepSeek chats…",
		terms = "import deepseek export zip chats read-only",
	},
})

-- What the host's promise said, in words (the engine's own on a refusal).
local function say(err: any): string
	return if type(err) == "table" and type(err.message) == "string" then err.message else tostring(err)
end

-- One import at a time: busy while the engine reads the ZIP, the answer it
-- gave, and the engine's sentence when it refused.
local busy = ui.state("settings_import_busy", false)
local answer = ui.state("settings_import_answer", nil :: any?)
local problem = ui.state("settings_import_problem", nil :: string?)
-- The path last picked: Try again sends it once more.
local chosen = ui.state("settings_import_path", nil :: string?)

local function run(path: string)
	busy:set(true)
	problem:set(nil)
	host.call("settings.engine", { method = "ImportDeepSeekChats", params = { path = path } }):after(function(ok: any)
		busy:set(false)
		answer:set(ok)
	end, function(err: any)
		busy:set(false)
		problem:set(say(err))
	end)
end

local function choose()
	if busy:get() then
		return
	end
	answer:set(nil)
	problem:set(nil)
	host.open_files({ title = ui.t("settings.import_chats.choose"):get(), multiple = false }):after(function(files: any)
		-- (Cancelled: the list is empty, and the engine is asked nothing.)
		local f = files and files[1]
		if not f then
			return
		end
		chosen:set(f.path)
		run(f.path)
	end, function(err: any)
		problem:set(say(err))
	end)
end

local function retry()
	local path = chosen:get()
	if path and not busy:get() then
		run(path)
	end
end

-- The engine's counts, as it answered them.
local function count(field: string): number
	local a = answer:get()
	return if type(a) == "table" then tonumber(a[field]) or 0 else 0
end

function M.view(): Node
	return ds.Settings.Page {
		title = ui.t("settings.import_chats.title"),
		description = ui.t("settings.import_chats.description"),
		ds.Settings.Section {
			ds.Settings.Group {
				search.anchor("import", ds.Settings.Row {
					label = ui.t("settings.import_chats.action.label"),
					description = ui.t("settings.import_chats.action.description"),
					control = ds.Button {
						id = "settings-import-chats",
						ui.t("settings.import_chats.choose"),
						size = "sm",
						tone = "secondary",
						busy = busy,
						disabled = busy,
						on_click = choose,
					},
				}),
			},
		},
		-- What it did, in the engine's counts.
		ui.show(function(): boolean
			return answer:get() ~= nil
		end, ds.Settings.Section {
			ds.Settings.Group {
				ds.Settings.Row {
					id = "settings-import-imported",
					label = function(): string
						return ui.t("settings.import_chats.imported", { n = count("imported") }):get()
					end,
					description = function(): string
						return ui.t("settings.import_chats.already", { n = count("alreadyPresent") }):get()
					end,
				},
				ui.show(function(): boolean
					return count("total") > count("kept")
				end, ds.Settings.Row {
					id = "settings-import-kept",
					label = function(): string
						return ui.t("settings.import_chats.kept", { kept = count("kept"), total = count("total") }):get()
					end,
				}),
			},
		}),
		-- A refusal, in the engine's words, with the same path to try again.
		ui.show(function(): boolean
			return problem:get() ~= nil
		end, ds.Settings.Section {
			ds.Settings.Group {
				ds.Settings.Row {
					id = "settings-import-failed",
					label = ui.t("settings.import_chats.failed"),
					description = function(): string
						return problem:get() or ""
					end,
					control = ds.Button {
						id = "settings-import-retry",
						ui.t("settings.import_chats.retry"),
						size = "sm",
						tone = "secondary",
						disabled = busy,
						on_click = retry,
					},
				},
			},
		}),
	}
end

return M
