local ds = require("@ds")
local providers = require("@app/screens/settings/providers")

export type Props = { step: number?, count: number?, on_back: () -> (), on_continue: () -> () }

return function(props: Props): Node
	return ds.Onboarding.ScrollStep {
		place = "providers",
		step = props.step,
		count = props.count,
		back = ds.Onboarding.Back { on_click = props.on_back },
		continue = { label = "Continue", on_click = props.on_continue },
		form = { measure = ds.Onboarding.LIST_MEASURE, title = "Set up DeepSeek", body = "Use the official DeepSeek Harness SDK on your run device. You can configure it now or later in Settings → DeepSeek." },
		providers.body(),
	}
end
