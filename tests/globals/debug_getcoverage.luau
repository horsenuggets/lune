-- Test debug.getcoverage without coverage enabled
-- Should return empty tables since coverage is not instrumented

-- debug.getcoverage should exist as a function
assert(type(debug.getcoverage) == "function", "debug.getcoverage should be a function")

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
