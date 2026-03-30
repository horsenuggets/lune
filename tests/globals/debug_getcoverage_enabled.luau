-- Test debug.getcoverage and debug.iscoverageenabled with coverage on
-- Coverage is enabled by default, no env var needed

local Module = require("./coverage/module")

-- iscoverageenabled should return true
assert(debug.iscoverageenabled() == true, "iscoverageenabled should return true when coverage is enabled")

-- Exercise some functions
Module.greet("World")
Module.outer()

------------------------------------------------------------
-- Individual function returns exactly 1 entry
------------------------------------------------------------

local greetCoverage = debug.getcoverage(Module.greet)
assert(#greetCoverage == 1, `individual function should return 1 entry, got {#greetCoverage}`)
assert(greetCoverage[1].Function == "greet", "entry should be named 'greet'")

------------------------------------------------------------
-- Verify entry structure and field types
------------------------------------------------------------

local entry = greetCoverage[1]
assert(type(entry.Function) == "string", "Function should be a string")
assert(type(entry.LineDefined) == "number", "LineDefined should be a number")
assert(type(entry.Depth) == "number", "Depth should be a number")
assert(type(entry.Hits) == "table", "Hits should be a table")

for _, hit in entry.Hits do
	assert(type(hit) == "number", "each hit should be a number")
	assert(hit == math.floor(hit), "each hit should be an integer")
end

------------------------------------------------------------
-- Verify hit counts: -1 for non-executable, 0 for missed, >0 for hit
------------------------------------------------------------

-- greet was called, should have positive hits
local hasPositiveHit = false
for _, hit in entry.Hits do
	assert(hit >= -1, "hit count should be >= -1")
	if hit > 0 then
		hasPositiveHit = true
	end
end
assert(hasPositiveHit, "greet was called, should have at least one positive hit")

-- greet should have at least one non-executable line (-1)
local hasNegativeOne = false
for _, hit in entry.Hits do
	if hit == -1 then
		hasNegativeOne = true
		break
	end
end
assert(hasNegativeOne, "greet should have at least one non-executable line (-1)")

------------------------------------------------------------
-- Unused function should have zero hits on all executable lines
------------------------------------------------------------

local unusedCoverage = debug.getcoverage(Module.unused)
assert(#unusedCoverage == 1, "unused should return 1 entry")
for _, hit in unusedCoverage[1].Hits do
	assert(hit <= 0, "unused function should have no positive hits")
end

------------------------------------------------------------
-- Nested function: outer returns 2 entries (outer + inner)
------------------------------------------------------------

local outerCoverage = debug.getcoverage(Module.outer)
assert(#outerCoverage == 2, `outer should return 2 entries (outer + inner), got {#outerCoverage}`)

local outerEntry, innerEntry
for _, e in outerCoverage do
	if e.Function == "outer" then
		outerEntry = e
	elseif e.Function == "inner" then
		innerEntry = e
	end
end

assert(outerEntry, "should find outer entry")
assert(innerEntry, "should find inner entry")
assert(innerEntry.Depth > outerEntry.Depth, "inner should have greater depth than outer")

-- Both were called via Module.outer(), both should have hits
for _, hit in outerEntry.Hits do
	if hit > 0 then
		break
	end
end

local innerHasHit = false
for _, hit in innerEntry.Hits do
	if hit > 0 then
		innerHasHit = true
		break
	end
end
assert(innerHasHit, "inner was called via outer(), should have hits")

------------------------------------------------------------
-- Verify hit accumulation across multiple calls
------------------------------------------------------------

Module.greet("Alice")
Module.greet("Bob")
local afterCoverage = debug.getcoverage(Module.greet)
for _, hit in afterCoverage[1].Hits do
	if hit > 0 then
		assert(hit == 3, `greet was called 3 times total, expected 3, got {hit}`)
	end
end

------------------------------------------------------------
-- Built-in functions return empty table even with coverage on
------------------------------------------------------------

local builtinCoverage = debug.getcoverage(print)
assert(type(builtinCoverage) == "table", "built-in coverage should be a table")
assert(#builtinCoverage == 0, "built-in functions should have no coverage entries")
