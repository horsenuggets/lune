-- Test debug.getcoverage and debug.iscoverageenabled without coverage
-- Requires LUNE_COVERAGE=0 to be set

-- Both functions should exist
assert(type(debug.getcoverage) == "function", "debug.getcoverage should be a function")
assert(type(debug.iscoverageenabled) == "function", "debug.iscoverageenabled should be a function")

-- iscoverageenabled should return false when LUNE_COVERAGE=0
assert(debug.iscoverageenabled() == false, "iscoverageenabled should return false when coverage is disabled")

-- Without coverage enabled, should return an empty table
local function simpleFunction()
	return 1 + 1
end

simpleFunction()

local result = debug.getcoverage(simpleFunction)
assert(type(result) == "table", "debug.getcoverage should return a table")
assert(#result == 0, "debug.getcoverage should return empty table when coverage is not enabled")

-- Same for built-in functions
local builtinResult = debug.getcoverage(print)
assert(type(builtinResult) == "table", "debug.getcoverage on built-in should return a table")
assert(#builtinResult == 0, "debug.getcoverage on built-in should return empty table")
