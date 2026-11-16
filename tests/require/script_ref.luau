-- Test requiring modules via script references

-- Test 1: Require a sibling module using script.Parent with GetFullName
print("Testing require(script.Parent.script_ref_module:GetFullName())...")
const module = require(script.Parent.script_ref_module:GetFullName())

assert(module ~= nil, "Module should not be nil")
assert(type(module) == "table", "Module should be a table")
assert(module.message == "Hello from script_ref_module!", "Module message should match")
assert(module.getValue() == 42, "Module getValue should return 42")

print("Module loaded successfully!")
print("Module message:", module.message)
print("Module getValue():", module.getValue())

-- Test 2: Require using the script global directly (should be same as script.Path)
print("\nTesting that script path is correct...")
const scriptPath = tostring(script)
print("Script path:", scriptPath)
assert(
	string.find(scriptPath, "script_ref", 1, true) ~= nil,
	"Script path should contain 'script_ref'"
)

-- Test 3: Require a .lua file through its actual source path.
print("\nTesting .lua extension support...")
const luaModule = require(script.Parent.lua_module:GetFullName())
assert(luaModule ~= nil, ".lua module should load")
assert(luaModule.fromLua == true, ".lua module should have fromLua = true")
print(".lua module message:", luaModule.message)

-- Test 4: Require a folder with init.luau
print("\nTesting init.luau support (folder as module)...")
const initModule = require(script.Parent.init_test:GetFullName())
assert(initModule ~= nil, "init.luau module should load")
assert(initModule.fromInit == true, "init.luau module should have fromInit = true")
print("init.luau module message:", initModule.message)

-- Exact paths and ordinary references should share the same module cache.
const fs = require("@lune/fs")
for _, reference in
	{ script.Parent.script_ref_module, script.Parent.lua_module, script.Parent.init_test }
do
	const path = reference:GetFullName()
	assert(fs.isFile(path), "GetFullName must identify an existing source file")
	assert(#fs.readFile(path) > 0, "source paths must be directly readable")
	assert(require(reference) == require(path), "absolute and reference requires must agree")
end
assert(script.Parent.init_test:GetFullName():match("init%.luau$") ~= nil)
assert(script.Parent.lua_module:GetFullName():match("%.lua$") ~= nil)

-- Child names mapped by a project file should expose the actual source paths.
const mapped = script.Parent.project_test.Shared.Utils
assert(mapped:GetFullName():match("Utils%.luau$") ~= nil)
assert(#fs.readFile(mapped:GetFullName()) > 0)
assert(require(mapped) == require(mapped:GetFullName()))

print("\nAll require tests with script references passed!")
