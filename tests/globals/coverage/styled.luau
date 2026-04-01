-- A module with local helper functions that are not exported.
-- Used to test file-level coverage via debug.getcoverage(script).

local function formatName(name: string)
	return "[" .. name .. "]"
end

local function buildGreeting(prefix: string, name: string)
	return prefix .. " " .. formatName(name)
end

local Styled = {}

function Styled.hello(name: string)
	return buildGreeting("Hello", name)
end

function Styled.goodbye(name: string)
	return buildGreeting("Goodbye", name)
end

function Styled.unused()
	return buildGreeting("Hey", "nobody")
end

return Styled
