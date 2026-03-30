-- Test debug.getcoverage with LUNE_COVERAGE=1 (global coverage mode)
-- This test must be run with the LUNE_COVERAGE=1 env var set

local Module = require("./coverage/module")

-- Call one function but not the other
local greeting = Module.greet("World")
assert(greeting == "Hello, World", "greet should return greeting")

-- Get coverage from the called function
local greetCoverage = debug.getcoverage(Module.greet)
assert(#greetCoverage > 0, "greet should have coverage data with LUNE_COVERAGE=1")

local hasHit = false
for _, entry in greetCoverage do
	for _, hit in entry.Hits do
		if hit > 0 then
			hasHit = true
			break
		end
	end
end
assert(hasHit, "greet should show hits after being called")

-- Get coverage from the uncalled function
local farewellCoverage = debug.getcoverage(Module.farewell)
assert(#farewellCoverage > 0, "farewell should have coverage data with LUNE_COVERAGE=1")

for _, entry in farewellCoverage do
	for _, hit in entry.Hits do
		if hit >= 0 then
			assert(hit == 0, "farewell should have no hits since it was never called")
		end
	end
end
