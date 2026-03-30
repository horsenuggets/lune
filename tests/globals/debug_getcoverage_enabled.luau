-- Test debug.getcoverage with LUNE_COVERAGE=1 and luau.compile coverageLevel

local luau = require("@lune/luau")

-- Compile a module with coverage enabled via luau.compile
local source = [[
local M = {}

function M.add(a, b)
	return a + b
end

function M.unused()
	return "never called"
end

return M
]]

local bytecode = luau.compile(source, {
	coverageLevel = 1,
	debugLevel = 1,
	optimizationLevel = 0,
})
local moduleFunction = luau.load(bytecode, { debugName = "TestModule" })

-- Execute the module to get the table
local M = moduleFunction()

-- Call one function but not the other
local result = M.add(2, 3)
assert(result == 5, "add should return 5")

-- Get coverage from the module-level function (includes all inner functions)
local coverage = debug.getcoverage(moduleFunction)
assert(type(coverage) == "table", "coverage should be a table")
assert(#coverage > 0, "coverage should have entries when coverage is enabled")

-- Verify structure of coverage entries
for _, entry in coverage do
	assert(type(entry["function"]) == "string", "entry should have a function name string")
	assert(type(entry.lineDefined) == "number", "entry should have lineDefined number")
	assert(type(entry.depth) == "number", "entry should have depth number")
	assert(type(entry.hits) == "table", "entry should have hits table")
end

-- Find the add function and verify it was hit
local foundAdd = false
local foundUnused = false

for _, entry in coverage do
	if entry["function"] == "add" then
		foundAdd = true
		-- At least one hit should be > 0
		local hasHit = false
		for _, hit in entry.hits do
			if hit > 0 then
				hasHit = true
				break
			end
		end
		assert(hasHit, "add function should have at least one line hit")
	end

	if entry["function"] == "unused" then
		foundUnused = true
		-- All executable lines should be 0 (not hit)
		for _, hit in entry.hits do
			if hit >= 0 then
				assert(hit == 0, "unused function should have no lines hit")
			end
		end
	end
end

assert(foundAdd, "should find coverage entry for add function")
assert(foundUnused, "should find coverage entry for unused function")

-- Get coverage from an individual function
local addCoverage = debug.getcoverage(M.add)
assert(type(addCoverage) == "table", "individual function coverage should be a table")
assert(#addCoverage > 0, "individual function coverage should have entries")
assert(addCoverage[1]["function"] == "add", "individual function should report its name")

-- Verify hit counts are integers
for _, entry in coverage do
	for _, hit in entry.hits do
		assert(type(hit) == "number", "hit count should be a number")
		assert(hit == math.floor(hit), "hit count should be an integer")
	end
end

-- Test that calling a function multiple times increases hit counts
M.add(1, 2)
M.add(3, 4)
local coverageAfter = debug.getcoverage(M.add)
for _, entry in coverageAfter do
	if entry["function"] == "add" then
		for _, hit in entry.hits do
			if hit > 0 then
				assert(hit == 3, "add should have been called 3 times total")
			end
		end
	end
end
