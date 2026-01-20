-- Test requiring modules via script references

-- Test 1: Require a sibling module using script.Parent with RequirePath
print("Testing require(script.Parent.script_ref_module:RequirePath())...")
local module = require(script.Parent.script_ref_module:RequirePath())

assert(module ~= nil, "Module should not be nil")
assert(type(module) == "table", "Module should be a table")
assert(module.message == "Hello from script_ref_module!", "Module message should match")
assert(module.getValue() == 42, "Module getValue should return 42")

print("Module loaded successfully!")
print("Module message:", module.message)
print("Module getValue():", module.getValue())

-- Test 2: Require using the script global directly (should be same as script.Path)
print("\nTesting that script path is correct...")
local scriptPath = tostring(script)
print("Script path:", scriptPath)
assert(
	string.find(scriptPath, "script_ref", 1, true) ~= nil,
	"Script path should contain 'script_ref'"
)

-- Test 3: Require a .lua file (without extension in path)
print("\nTesting .lua extension support...")
local luaModule = require(script.Parent.lua_module:RequirePath())
assert(luaModule ~= nil, ".lua module should load")
assert(luaModule.fromLua == true, ".lua module should have fromLua = true")
print(".lua module message:", luaModule.message)

-- Test 4: Require a folder with init.luau
print("\nTesting init.luau support (folder as module)...")
local initModule = require(script.Parent.init_test:RequirePath())
assert(initModule ~= nil, "init.luau module should load")
assert(initModule.fromInit == true, "init.luau module should have fromInit = true")
print("init.luau module message:", initModule.message)

print("\nAll require tests with script references passed!")
