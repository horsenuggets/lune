-- Test the project global which refers to the nearest Git repository containing the script

local fs = require("@lune/fs")
local task = require("@lune/task")

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
	tostring(project.tests.Parent) == projectPath and project.tests.Name == "tests",
	"project.tests should be a child of the root"
)
assert(
	tostring(project.tests.globals) == tostring(script.Parent),
	"project.tests.globals should match script.Parent"
)
assert(
	tostring(project.Parent[project.Name]) == projectPath,
	"project.Parent and project.Name should split the root path"
)

-- Required modules should see the same project, both at load time and when called later
local module = require("./project_fixtures/module")
assert(module.LoadTimePath == projectPath, "module project should match at load time")
assert(module.getPath() == projectPath, "module project should match when called later")

-- Turn the nested fixture folder into its own repository for the rest of the test
local nestedRoot = tostring(script.Parent.project_fixtures.nested)
local nestedGitFile = nestedRoot .. "/.git"
fs.writeFile(nestedGitFile, "gitdir: ../nowhere\n")

-- Start loading a module from the nested repository and let it yield partway through, so
-- its path sits on the require stack while this thread keeps running
local nestedProject = nil
task.spawn(function()
	nestedProject = require("./project_fixtures/nested/module")
end)
task.wait(0.1)
local projectDuringRequire = tostring(project)
local scriptDuringRequire = tostring(script)
while nestedProject == nil do
	task.wait(0.05)
end

-- Clean up before asserting so a failure doesn't leave a stray repository behind
fs.removeFile(nestedGitFile)

assert(nestedProject == nestedRoot, "a module in a nested repository should see that repository")
assert(
	projectDuringRequire == projectPath,
	"project should ignore modules other threads are loading"
)
assert(
	scriptDuringRequire == tostring(script),
	"script should ignore modules other threads are loading"
)
