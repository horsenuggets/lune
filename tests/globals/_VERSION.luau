-- _VERSION global should follow the following format:
--[[
	_VERSION global must have the following format (for this fork):

	Lune LUNE_MAJOR.LUNE_MINOR.LUNE_PATCH-horse.FORK_VERSION

	Examples:

	Lune 0.10.4-horse.0.1
	Lune 0.10.4-horse.0.3

	Note: This fork uses "-" instead of "+" for Wally compatibility.
]]

assert(_VERSION ~= nil, "_VERSION global is missing")
assert(type(_VERSION) == "string", "_VERSION global must be a string")

assert(string.sub(_VERSION, 1, 5) == "Lune ", "_VERSION global must start with 'Lune '")

local withoutPrefix = string.sub(_VERSION, 6)

-- For this fork, split on "-" instead of "+"
local versions = string.split(withoutPrefix, "-")
assert(versions[1] ~= nil, "_VERSION global does not contain lune version")
-- Fork version is optional but expected in this fork
assert(versions[2] ~= nil, "_VERSION global does not contain fork version (expected 'horse.X.Y')")

local luneVersion = string.split(versions[1], ".")
assert(luneVersion[1] ~= nil, "_VERSION global is missing lune major version")
assert(luneVersion[2] ~= nil, "_VERSION global is missing lune minor version")
assert(luneVersion[3] ~= nil, "_VERSION global is missing lune patch version")

assert(tonumber(luneVersion[1]) ~= nil, "_VERSION global lune major version is not a number")
assert(tonumber(luneVersion[2]) ~= nil, "_VERSION global lune minor version is not a number")
assert(tonumber(luneVersion[3]) ~= nil, "_VERSION global lune patch version is not a number")

-- Check that fork version starts with "horse."
assert(
	string.sub(versions[2], 1, 6) == "horse.",
	"_VERSION global fork version should start with 'horse.'"
)

print("_VERSION:", _VERSION)
print("Lune version:", versions[1])
print("Fork version:", versions[2])
