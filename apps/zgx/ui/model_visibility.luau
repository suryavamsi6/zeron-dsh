-- Which models the composer offers. A route's models can be switched off on
-- the Providers page (Settings → Providers → Models), and the choice is kept
-- across launches.
--
-- A model's route is its `provider.id` (the catalog's `ModelProvider`); the
-- agent's own models carry none and are never hidden — they are what a chat
-- runs with no pick at all.
local M = {}

local hidden = ui.persist("model_routes_hidden", {} :: { [string]: boolean })

-- The route a model is served from, or nil for the agent's own.
function M.route(model: any): string?
	local provider = if type(model) == "table" then model.provider else nil
	if type(provider) ~= "table" then
		return nil
	end
	local id = provider.id
	return if type(id) == "string" and id ~= "" then id else nil
end

-- Whether a route's models are shown.
function M.showing(route: string?): boolean
	return route == nil or hidden:get()[route] ~= true
end

-- `models`, without those whose route is switched off.
function M.visible(models: { any }): { any }
	local now = hidden:get()
	if next(now) == nil then
		return models
	end
	local out = {}
	for _, model in models do
		if now[M.route(model)] ~= true then
			table.insert(out, model)
		end
	end
	return out
end

-- Switch a route's models off (`off`) or back on.
function M.set_showing(route: string, on: boolean)
	local now = table.clone(hidden:peek())
	now[route] = if on then nil else true
	hidden:set(now)
end

return M
