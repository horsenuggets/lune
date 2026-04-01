-- Test debug.getcoverage with script instances for file-level coverage.
-- When passed a script reference instead of a function, getcoverage
-- returns coverage for ALL functions in the file, including locals.

local Styled = require("./coverage/styled")

-- Exercise some functions (but not all)
Styled.hello("World")
Styled.goodbye("World")

------------------------------------------------------------
-- Script-based coverage returns entries for ALL functions
------------------------------------------------------------

-- script.Parent is the directory containing this test file.
-- script.Parent.coverage.styled points to the styled.luau module.
local styledScript = script.Parent.coverage.styled

local coverage = debug.getcoverage(styledScript)

-- The module has 5 functions:
-- 1. top-level chunk (the module itself)
-- 2. formatName (local)
-- 3. buildGreeting (local)
-- 4. Styled.hello
-- 5. Styled.goodbye
-- 6. Styled.unused
assert(#coverage >= 5, `expected at least 5 coverage entries, got {#coverage}`)

------------------------------------------------------------
-- Local functions should be visible (not possible with
-- function-based coverage since they are not exported)
------------------------------------------------------------

local foundFormatName = false
local foundBuildGreeting = false
for _, entry in coverage do
	if entry.Function == "formatName" then
		foundFormatName = true
		-- formatName was called (via hello and goodbye), should have hits
		local hasHit = false
		for _, hit in entry.Hits do
			if hit > 0 then
				hasHit = true
				break
			end
		end
		assert(hasHit, "formatName was called indirectly, should have hits")
	elseif entry.Function == "buildGreeting" then
		foundBuildGreeting = true
		local hasHit = false
		for _, hit in entry.Hits do
			if hit > 0 then
				hasHit = true
				break
			end
		end
		assert(hasHit, "buildGreeting was called indirectly, should have hits")
	end
end

assert(foundFormatName, "should find local function 'formatName' in file-level coverage")
assert(foundBuildGreeting, "should find local function 'buildGreeting' in file-level coverage")

------------------------------------------------------------
-- Unused function should have zero hits
------------------------------------------------------------

local foundUnused = false
for _, entry in coverage do
	if entry.Function == "unused" then
		foundUnused = true
		for _, hit in entry.Hits do
			assert(hit <= 0, "unused function should have no positive hits")
		end
	end
end
assert(foundUnused, "should find 'unused' in file-level coverage")

------------------------------------------------------------
-- Exported function coverage should also be present
------------------------------------------------------------

local foundHello = false
for _, entry in coverage do
	if entry.Function == "hello" then
		foundHello = true
		local hasHit = false
		for _, hit in entry.Hits do
			if hit > 0 then
				hasHit = true
				break
			end
		end
		assert(hasHit, "hello was called, should have hits")
	end
end
assert(foundHello, "should find exported function 'hello' in file-level coverage")

print("All script-based coverage tests passed")
