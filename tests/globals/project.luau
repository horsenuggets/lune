-- Test the project global which refers to the nearest Git repository containing the script

local fs = require("@lune/fs")

-- project global should exist and behave like script
assert(project ~= nil, "project global is missing")
assert(typeof(project) == "ScriptReference", "project typeof should be 'ScriptReference'")

-- This file lives in the Lune repository, so project should be its root
local projectPath = tostring(project)
assert(#projectPath > 0, "tostring(project) should not be empty")
assert(fs.isFile(projectPath .. "/Cargo.toml"), "project should be the repository root")
assert(
	fs.isDir(projectPath .. "/.git") or fs.isFile(projectPath .. "/.git"),
	"project should contain a .git entry"
)
assert(project:GetFullName() == projectPath, "project:GetFullName() should equal tostring(project)")

-- Children should resolve relative to the repository root, regardless of script location
assert(
	tostring(project.tests) == projectPath .. "/tests",
	"project.tests should be a child of the root"
)
assert(
	tostring(project.tests.globals) == tostring(script.Parent),
	"project.tests.globals should match script.Parent"
)
assert(
	tostring(project.Parent) .. "/" .. project.Name == projectPath,
	"project.Parent and project.Name should split the root path"
)

-- Required modules should see the same project, both at load time and when called later
local module = require("./project/module")
assert(module.LoadTimePath == projectPath, "module project should match at load time")
assert(module.getPath() == projectPath, "module project should match when called later")
