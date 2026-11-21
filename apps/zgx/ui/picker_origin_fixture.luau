-- A model picker on models made up here, for
-- tests/model_picker_origin.test.luau: one row of each kind the picker can
-- be given — the runtime's own model (no provider, no description), an
-- account-sourced row (a provider, no description), and a row the engine
-- could only fill from the shipped catalog (a provider *and* a description
-- saying why). The picker draws only the one runtime's models
-- (`model_picker.luau`'s `model_list`), so all three are its own. The model
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

local MODELS: { ModelPicker.Model } = {
	{ value = "deepseek/deepseek-flash", label = "DeepSeek V4.1 Flash", harness = "deepseek", efforts = THREE },
	{ value = "deepseek/zai-glm-5", label = "Zai GLM 5", harness = "deepseek", efforts = THREE, provider = { id = "openrouter", name = "OpenRouter" } },
	{ value = "deepseek/mistral-medium-3", label = "Mistral Medium 3", harness = "deepseek", efforts = THREE, provider = { id = "openrouter", name = "OpenRouter" }, description = "Shipped catalog — 401" },
}

local HARNESSES: { ModelPicker.Harness } = {
	{ id = "deepseek", label = "DeepSeek", mark = "provider-deepseek" },
}

return ui.column {
	w_full = true,
	h_full = true,
	p = "$space.4",
	ModelPicker {
		model = ui.state("test_picker_model", "deepseek/zai-glm-5" :: string?),
		effort = ui.state("test_picker_effort", "medium" :: string?),
		favorites = ui.state("test_picker_favorites", {} :: { [string]: boolean }),
		open_with = ui.state("test_picker_open", 0),
		harnesses = HARNESSES,
		models = MODELS,
	},
}
