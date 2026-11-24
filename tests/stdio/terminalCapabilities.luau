--[[

terminalCapabilities

Checks the native capability result's shape, immutability, and repeatability.

--]]

const stdio = require("@lune/stdio")

const capabilities = stdio.terminalCapabilities()
assert(type(capabilities) == "table")
assert(type(capabilities.ForegroundOpacity) == "boolean")
assert(table.isfrozen(capabilities))
assert(not pcall(function()
	capabilities.ForegroundOpacity = not capabilities.ForegroundOpacity
end))

const again = stdio.terminalCapabilities()
assert(again.ForegroundOpacity == capabilities.ForegroundOpacity)
