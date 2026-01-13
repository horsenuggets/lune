-- Test script that requires shared modules via project.json paths
print("Testing default.project.json support...")

-- Navigate to project root and then to Shared
local projectRoot = script.Parent.Parent.Parent  -- src/server -> src -> project_test
print("Project root:", projectRoot)

-- Access Shared via the project tree
local Shared = projectRoot.Shared
print("Shared path:", Shared)

-- Require Utils from Shared
local Utils = require(Shared.Utils:RequirePath())
assert(Utils ~= nil, "Utils should not be nil")
assert(Utils.name == "Utils", "Utils.name should be 'Utils'")

local greeting = Utils.greet("World")
assert(greeting == "Hello, World!", "Greeting should match")
print("Greeting:", greeting)

print("default.project.json test passed!")
