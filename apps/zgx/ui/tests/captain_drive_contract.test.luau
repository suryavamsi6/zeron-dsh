-- SUR-250: the Drive fixture answers the host's contract, not a hand-written
-- copy of what the page wants.
--
-- `captain_drive_fixture.luau` stands in for `apps/zgx/src/captains.rs`'s two
-- actions, and an invented answer keeps the suite green whatever the host
-- sends: measured on SUR-92's branch, the host listed a folder as `kind =
-- "dir"` and gave `name` as a base name, and every case here passed. The
-- fixture now answers `captain_drive_contract.luau`'s rows and reads; these
-- cases pin the shapes the host can send, so a contract edited away from them
-- fails at the fixture's mount (or here), while the cases that render the tab
-- keep running over the same answers.
--
-- What these cases cannot see is the host changing under the contract: that
-- half is `captains_tests.rs`'s, where the real `drive_files`/`drive_read` run.

local function home(app: any)
	app:resize(1100, 780)
	app:frames(30)
end

-- The Drive's answers as the fixture serves them (`capture_drive`, which the
-- stub itself answers from). A contract the host could not send is refused at
-- the mount, and named here rather than as a missing command.
local function capture(app: any): any
	local mounted = app:state("captain_drive_capture")
	if mounted and mounted.problems then
		error("the fixture refused its own contract: " .. table.concat(mounted.problems, "; "))
	end
	app:command("capture_drive")
	local c = app:state("captain_drive_capture")
	expect(c ~= nil).to_be(true)
	return c
end

-- A table's keys, sorted, for "these and no others".
local function keys(t: any): string
	local out: { string } = {}
	for k in t do
		table.insert(out, tostring(k))
	end
	table.sort(out)
	return table.concat(out, ",")
end

test("captain_drive_contract: the fixture answers the host's listing, row for row", function()
	local app = mount("captain_drive_fixture.luau")
	home(app)
	local c = capture(app)
	-- The host's rows (`drive_files`): the Drive's top level but `state/`,
	-- then the files directly under `reports/` — sorted by name, a file with
	-- `size` and RFC 3339 `mtime`, a directory with neither.
	local golden = {
		{ "AGENTS.md", "file" },
		{ "CREW.md", "file" },
		{ "DIGEST.md", "file" },
		{ "NOTES.md", "file" },
		{ "handoffs", "directory" },
		{ "reports", "directory" },
		{ "reports/a2.md", "file" },
		-- (The one the host never sends, added to prove the tab hides it.)
		{ "state/needs-you.json", "file" },
	}
	expect(#c.files).to_be(#golden)
	local last = ""
	for i, row in c.files do
		expect(row.name).to_be(golden[i][1])
		expect(row.kind).to_be(golden[i][2])
		if row.kind == "file" then
			expect(type(row.size)).to_be("number")
			expect(type(row.mtime)).to_be("string")
			expect(string.match(row.mtime, "^%d%d%d%d%-%d%d%-%d%dT%d%d:%d%d:%d%dZ$") ~= nil).to_be(true)
		else
			-- A directory carries neither, as the host sends it.
			expect(row.size).to_be(nil)
			expect(row.mtime).to_be(nil)
		end
		expect(row.name > last).to_be(true)
		last = row.name
	end
end)

test("captain_drive_contract: a read answers the host's two keys, and nothing else", function()
	local app = mount("captain_drive_fixture.luau")
	home(app)
	local c = capture(app)
	local served = 0
	for _, answer in c.reads do
		served = served + 1
		expect(answer ~= nil).to_be(true)
		-- `drive_read`'s answer is `{ text, size }`; a `path` is the page's
		-- own (it keeps the one it asked for), never the host's.
		expect(keys(answer)).to_be("size,text")
		expect(answer.path).to_be(nil)
		expect(type(answer.text)).to_be("string")
		expect(answer.size).to_be(#answer.text)
	end
	-- Every file the contract holds text for, including the one inside
	-- `reports/`, answered a read (`state/`'s row is machine-only and never
	-- opened).
	expect(served).to_be(5)
	expect(c.reads["reports/a2.md"] ~= nil).to_be(true)
end)

test("captain_drive_contract: the host's refusals, word for word", function()
	local app = mount("captain_drive_fixture.luau")
	home(app)
	local c = capture(app)
	-- `drive_read`'s own two: one for a path out of the Drive, one for a file
	-- that is not in it — and a file inside a folder by its bare name is the
	-- second, because the host takes the Drive-relative path.
	expect(c.escape).to_be("The path leaves the Captain's Drive")
	expect(c.missing).to_be("The file isn't in the Captain's Drive")
	expect(c.bare).to_be("The file isn't in the Captain's Drive")
end)
