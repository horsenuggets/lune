const CoverageFixture = require("../luau/coverage_fixture")
const actor = require("@lune/actor")

-- Actors return their raw coverage data, so owners can merge it exactly

if not debug.iscoverageenabled() then
	return
end

const ok, result = actor.spawn("./workers/coverage.luau"):join()
assert(ok, result)
CoverageFixture.assertSnapshots(result.Snapshots)
const entries = result.Measured
assert(#entries > 0, "Coverage entries should be returned")

local hits = 0
for _, entry in entries do
	assert(type(entry.Function) == "string", "Entries should name their function")
	assert(type(entry.LineDefined) == "number", "Entries should keep their line")
	for _, count in entry.Hits do
		hits += math.max(count, 0)
	end
end
assert(hits > 0, "Coverage from the actor's own executions should be recorded")
