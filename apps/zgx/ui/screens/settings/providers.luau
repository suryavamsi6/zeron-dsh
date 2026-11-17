local host = require("@app/screens/settings/host")
local ds = require("@ds")
local agents = require("@account/agents")
local devices = require("@app/screens/settings/devices")
local sessions = require("@account/sessions")
local visits = require("@app/screens/settings/visits")
local cloud = require("@account/cloud")
local search = require("@app/screens/settings/search")

local M = { ID = "providers", TITLE = "DeepSeek" }
search.index(M.ID, "DeepSeek", "DeepSeek SDK API key runtime integration authentication", {
	{ key = "agents", title = "DeepSeek setup", terms = "SDK API key" },
})

local function here(): string?
	for _, device in sessions.devices:get() or {} do
		if device.here then return device.id end
	end
	return nil
end

function M.body(): Node
	return ui.component(function(): Node
		local key = ui.use_state("")
		local busy = ui.use_state(false)
		local message = ui.use_state("")
		local function save()
			if busy:peek() or key:peek() == "" then return end
			local target = agents.now:peek().target
			if target ~= nil and target ~= "cloud" then
				message:set("Open DeepSeek settings on the run device to save its API key.")
				return
			end
			busy:set(true)
			message:set("")
			local request = if target == "cloud"
				then host.call("agents.cloud_key", { provider = "deepseek-key", key = key:peek(), device = cloud.device() or "" })
				else host.call("agents.add_key", { id = "deepseek", provider = "deepseek", key = key:peek() })
			request:after(function()
				key:set("")
				busy:set(false)
				message:set("DeepSeek API key saved for the next run.")
				agents.load()
			end, function()
				busy:set(false)
				message:set("Couldn't save the DeepSeek API key. Check the run device and try again.")
			end)
		end
		return ui.column {
			w_full = true,
			gap = "$space.6",
			ds.Settings.Section {
				title = "Official DeepSeek Harness SDK",
				description = "Install the pinned SDK runtime on the device that runs your sessions. Zeron uses an isolated Harness home, not your personal Harness configuration.",
				ds.Settings.Group {
					ds.Settings.Row {
						label = "SDK setup guide",
						description = "See the official SDK documentation for installation and authentication requirements.",
						control = ds.Button { "Open guide", tone = "secondary", size = "sm", on_click = function()
							host.invoke("open_url", "https://github.com/deepseek-ai/deepseek-harness")
						end },
					},
					ds.Settings.Row {
						label = "DeepSeek API key",
						description = "Provide DEEPSEEK_API_KEY securely to the engine, or save a key here. An explicitly saved key takes precedence. No browser provider sign-in is required.",
						stacked = true,
						control = ui.column {
							gap = "$space.3",
							ds.TextField { value = key, password = true, w_full = true, name = "DeepSeek API key", placeholder = "DeepSeek API key", on_commit = save },
							ds.Button { "Save DeepSeek API key", tone = "secondary", size = "sm", busy = function() return busy:get() end, disabled = function() return key:get() == "" end, on_click = save },
							ds.Text { function() return message:get() end, tone = "secondary", wrap = true },
						},
					},
				},
			},
			ds.Settings.Section {
				title = "Historical sessions",
				description = "Sessions and settings from other providers are preserved but unsupported. Choose a DeepSeek model explicitly before sending; Zeron never silently changes their configuration.",
			},
		}
	end)({})
end

function M.view(): Node
	visits.once("providers", function() agents.load() end)
	return ds.Settings.Page {
		title = "DeepSeek",
		description = "Configure the single DeepSeek integration on the run device.",
		trail = devices.picker(function() return agents.now:get().target or here() end, function(id)
			agents.target(if id == here() then nil else id)
		end, function()
			return if cloud.device() then { { value = "cloud", label = "Cloud", icon = "cloud" } } else {}
		end),
		trail_top = true,
		M.body(),
	}
end

return M
