-- Test the script global which provides the current script path

-- script global should exist
assert(script ~= nil, "script global is missing")

-- script is a ScriptReference userdata (typeof returns "ScriptReference")
assert(typeof(script) == "ScriptReference", "script typeof should be 'ScriptReference'")

-- tostring(script) should return the path to this test file
const scriptPath = tostring(script)
assert(type(scriptPath) == "string", "tostring(script) should return a string")
assert(#scriptPath > 0, "tostring(script) should not be empty")
print("Script path:", scriptPath)

-- The path should contain "globals/script" since this file is tests/globals/script.luau
assert(
	string.find(scriptPath, "globals/script", 1, true) ~= nil
		or string.find(scriptPath, "globals\\script", 1, true) ~= nil,
	"script path should contain 'globals/script'"
)

-- script.Name should return just the file name
const scriptName = script.Name
assert(type(scriptName) == "string", "script.Name should return a string")
assert(#scriptName > 0, "script.Name should not be empty")
print("Script name:", scriptName)
assert(string.find(scriptName, "script", 1, true) ~= nil, "script.Name should contain 'script'")

-- GetFullName returns the actual source, while tostring keeps its module path.
const fs = require("@lune/fs")
const scriptFullPath = script:GetFullName()
assert(type(scriptFullPath) == "string", "script:GetFullName() should return a string")
assert(
	scriptFullPath:gsub("\\", "/") == scriptPath:gsub("\\", "/") .. ".luau",
	"GetFullName should include the extension"
)
assert(fs.readFile(scriptFullPath):find("script global", 1, true) ~= nil)
assert(tostring(script) == scriptPath, "GetFullName must not change tostring")
assert(fs.isDir(script.Parent:GetFullName()), "ordinary folders should remain directories")
assert(not pcall(function()
	script.Parent.DoesNotExist:GetFullName()
end), "missing targets should error")
assert(not pcall(function()
	script:RequirePath()
end), "RequirePath must no longer be callable")

const roblox = require("@lune/roblox")
const folder = roblox.Instance.new("Folder")
folder.Name = "Root"
const child = roblox.Instance.new("Folder")
child.Name = "Child"
child.Parent = folder
assert(child:GetFullName() == "Root.Child", "Roblox hierarchy naming must be unchanged")
folder:Destroy()

-- String concatenation should work
const prefixed = "Path: " .. script
assert(type(prefixed) == "string", "string concatenation should work")
assert(string.find(prefixed, "Path: ", 1, true) == 1, "concatenation should preserve prefix")
print("Concatenated:", prefixed)

const suffixed = script .. " (test)"
assert(type(suffixed) == "string", "string concatenation should work (suffix)")
assert(string.find(suffixed, " (test)", 1, true) ~= nil, "concatenation should preserve suffix")
print("Concatenated (suffix):", suffixed)

-- print(script) should work (uses __tostring) and just show the path
print("Printing script directly:", script)

-- Test script.Parent navigation
const parent = script.Parent
assert(parent ~= nil, "script.Parent should exist")
assert(typeof(parent) == "ScriptReference", "script.Parent should be ScriptReference")
print("script.Parent:", parent)
print("script.Parent.Name:", parent.Name)
print("script.Parent:GetFullName():", parent:GetFullName())

-- Test navigation to sibling
const sibling = script.Parent.SomeSibling
assert(sibling ~= nil, "script.Parent.SomeSibling should exist as a reference")
print("script.Parent.SomeSibling:", sibling)
