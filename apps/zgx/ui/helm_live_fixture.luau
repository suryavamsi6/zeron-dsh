-- SUR-230 — the Captain's Needs-you strip and Digest tab over a live
-- in-process engine: `Captains`/`CaptainDrive` are the host's own actions
-- (`src/captains.rs`'s `install_over`), so the page reads the roster and
-- the Drive the engine wrote — no stubs. `src/helm_live.rs`'s `digest()`
-- mounts it; `helm_live_tests.rs` drives it after the wake lands.

ui.theme.strict("warn")
local ds = require("@ds")
ds.setup()

local store = require("@app/screens/chat/store")
local CaptainDigest = require("@app/screens/chat/captain_digest")

local CHAT = "a1"

store.session:set({ id = CHAT, cwd = "/proj" })
store.messages:set({ { id = "m1", role = "user", text = "What is waiting?" } })

return ui.column {
	w_full = true,
	h_full = true,
	min_h = 0,
	CaptainDigest.Strip {
		chat = CHAT,
		open = function(_chat: string) end,
	},
	CaptainDigest.Tab { chat = CHAT },
}
