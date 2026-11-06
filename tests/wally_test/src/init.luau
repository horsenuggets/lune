--[[

WallyTest

Tests requiring Wally packages using the script.Parent pattern, simulating how
Roblox code requires packages.

--]]

print("Testing Wally package imports with script global...")

-- script = tests/wally_test/src (the directory containing init.luau)
-- script.Parent = tests/wally_test (the project root where default.project.json lives)
print("Script:", script)
print("Script.Parent:", script.Parent)

-- Navigate to ReplicatedStorage.Packages using project.json paths
-- In the project.json:
--   - ReplicatedStorage.Packages -> Packages folder
--   - ServerScriptService.Main -> src folder
-- So from script.Parent (the project root), we can access ReplicatedStorage
const projectRoot = script.Parent -- src -> project root (where default.project.json is)
print("Project root:", projectRoot)

const ReplicatedStorage = projectRoot.ReplicatedStorage
print("ReplicatedStorage:", ReplicatedStorage)

const Packages = ReplicatedStorage.Packages
print("Packages:", Packages)

-- Try to require Signal using the script global pattern
const SignalModule = Packages.Signal
print("Signal module reference:", SignalModule)

-- Get the require path and require it
const requirePath = SignalModule:RequirePath()
print("Signal require path:", requirePath)

const Signal = require(requirePath)
print("Signal module loaded:", Signal)

-- Test basic Signal functionality
assert(Signal ~= nil, "Signal should not be nil")
assert(Signal.new ~= nil, "Signal.new should exist")

const signal = Signal.new()
assert(signal ~= nil, "Creating a new signal should work")
print("Created signal:", signal)

-- Test connecting and firing
local fired = false
local receivedValue = nil

signal:Connect(function(value)
	fired = true
	receivedValue = value
end)

signal:Fire("test value")

-- Give async handlers time to run
task.wait(0.01)

assert(fired, "Signal should have fired")
assert(receivedValue == "test value", `Expected "test value", got "{receivedValue}"`)

print("Signal tests passed!")

-- Test Promise package
-- Promise uses game:GetService("RunService").Heartbeat which is Roblox-specific
-- This demonstrates that the project.json $path mapping works (promise/lib is loaded)
-- but packages that depend on Roblox DataModel APIs won't work without modifications
print("\nTesting Promise package (expects to fail due to Roblox APIs)...")
const PromiseModule = Packages.Promise
print("Promise module reference:", PromiseModule)

const promiseSuccess, promiseResult = pcall(function()
	const requirePath = PromiseModule:RequirePath()
	print("Promise require path:", requirePath)
	return require(requirePath)
end)

if promiseSuccess then
	print("Promise module loaded successfully:", promiseResult)
	print("Promise tests passed!")
else
	-- Expected failure: Promise uses game:GetService() which doesn't exist in Lune
	const errorStr = tostring(promiseResult)
	if string.find(errorStr, "GetService") then
		print("Promise failed as expected (Roblox API dependency: game:GetService)")
		print("The default.project.json path mapping worked correctly!")
	else
		print("Promise failed with unexpected error:", promiseResult)
	end
end

print("\nAll tests completed!")
