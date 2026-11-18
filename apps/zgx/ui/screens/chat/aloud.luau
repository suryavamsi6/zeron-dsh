-- A finished reply read aloud: DeepSeek's own speech (the engine's
-- `ReadAloud`, its PCM played by the host: src/read_aloud.rs), so a chat on
-- that agent offers it beside a settled reply's Copy. One read at a time:
-- starting another stops the last, and its button turns to Stop reading
-- while it goes — the host's call itself lasts the read, answering when
-- the sound ends.
--
--   T.Transcript { aloud = Aloud.for_session(function() return store.session:get() end), … }

local Icon = require("@ds/icon")
local Tooltip = require("@ds/tooltip")
local toast = require("@ds/toast")
local control = require("@ds/control")
local store = require("@app/screens/chat/store")

local M = {}

-- The reply being read (its message id), nil while none. A reload mid-read
-- leaves no call behind to settle, so it starts cleared.
M.playing = ui.state("chat_aloud_playing", nil :: string?)
M.playing:set(nil)

local function press(m: any, row: any)
	if M.playing:peek() == m.id then
		store.stop_read_aloud():after(function() end, function() end)
		return
	end
	local say = row:peek().copy or row:peek().text
	if say == nil or say == "" then
		return
	end
	M.playing:set(m.id)
	local function clear()
		if M.playing:peek() == m.id then
			M.playing:set(nil)
		end
	end
	store.read_aloud(say):after(clear, function(err: any)
		clear()
		toast.show {
			title = ui.t("chat.aloud.failed"):get(),
			body = if type(err) == "table" then tostring(err.message) else tostring(err),
			tone = "warning",
		}
	end)
end

-- One reply's button: read its words, or stop while they're read.
local function button(m: any, row: any): Node
	local function reading(): boolean
		return M.playing:get() == m.id
	end
	return ui.button {
		label = function(): string
			return ui.t(if reading() then "chat.aloud.stop" else "chat.aloud.read"):get()
		end,
		tooltip = Tooltip { function(): string
			return ui.t(if reading() then "chat.aloud.stop" else "chat.aloud.read"):get()
		end },
		w = 24,
		h = 24,
		p = 0,
		items = "center",
		justify = "center",
		rounded = "$radius.md",
		bg = "#00000000",
		color = "$text.secondary",
		hover = { bg = "$zeron.secondary.bg" },
		active = { bg = "$zeron.secondary.hover" },
		focus = control.ring,
		cursor = "pointer",
		on_click = function()
			press(m, row)
		end,
		Icon {
			name = function(): string
				return if reading() then "stop" else "volume-loud"
			end,
			size = 14,
		},
	}
end

-- `aloud` for a transcript over the session `current()` reads: the action
-- beside a reply's Copy while its chat runs DeepSeek; elsewhere nothing.
function M.for_session(current: () -> any): (m: any, row: any) -> Node?
	return function(m: any, row: any): Node?
		if m.role ~= "assistant" then
			return nil
		end
		return ui.show(function(): boolean
			local s = current()
			return s ~= nil and s.harness == "deepseek"
		end, button(m, row))
	end
end

return M
