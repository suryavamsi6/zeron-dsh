local ds = require("@ds")
local providers = require("@app/screens/settings/providers")
local agents = require("@account/agents")
local state = require("./state")

export type Props = { step: number?, count: number?, on_back: () -> (), on_continue: () -> () }

return function(props: Props): Node
	-- (Once a visit: a hot reload doesn't ask again.)
	state.once_a_visit("providers", agents.load)
	return ds.Onboarding.ScrollStep {
		place = "providers",
		step = props.step,
		count = props.count,
		back = ds.Onboarding.Back { on_click = props.on_back },
		continue = { label = "Continue", on_click = props.on_continue },
		form = { measure = ds.Onboarding.LIST_MEASURE, title = "Connect a provider", body = "Zeron runs on the provider you configure here. You can do this now, or later in Settings → Providers." },
		providers.body(),
	}
end
