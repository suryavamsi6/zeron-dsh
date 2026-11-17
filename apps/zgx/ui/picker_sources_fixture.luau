-- A model picker on models made up here, for
-- tests/model_picker_sources.test.luau: one whose name is long, and one
-- short. The one runtime names no provider, so no row says one. The model
-- chosen is `test_picker_model`; `test_picker_open` bumped opens it. Here,
-- beside app.luau, as a mounted script loads modules from its own directory
-- down.

ui.theme.strict("warn")
local ds = require("@ds")
ds.setup()
local ModelPicker = require("@ds/model_picker")
local EffortSlider = require("@ds/effort_slider")

local THREE: { EffortSlider.Level } = {
	{ value = "low", label = "Low" },
	{ value = "medium", label = "Medium" },
	{ value = "high", label = "High" },
}
local LONG = "DeepSeek V4 Pro Max Preview Extended Context"

local MODELS: { ModelPicker.Model } = {
	{ value = "deepseek/deepseek-flash", label = "DeepSeek V4.1 Flash", harness = "deepseek", efforts = THREE },
	{ value = "deepseek/deepseek-v4-pro-max-preview", label = LONG, harness = "deepseek", efforts = THREE },
}

local HARNESSES: { ModelPicker.Harness } = {
	{ id = "deepseek", label = "DeepSeek", mark = "provider-deepseek" },
}

return ui.column {
	w_full = true,
	h_full = true,
	p = "$space.4",
	ModelPicker {
		model = ui.state("test_picker_model", "deepseek/deepseek-flash" :: string?),
		effort = ui.state("test_picker_effort", "medium" :: string?),
		favorites = ui.state("test_picker_favorites", {} :: { [string]: boolean }),
		open_with = ui.state("test_picker_open", 0),
		harnesses = HARNESSES,
		models = MODELS,
	},
}
