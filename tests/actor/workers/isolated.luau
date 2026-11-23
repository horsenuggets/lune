const actor = require("@lune/actor")
const roblox = require("@lune/roblox")
const stdio = require("@lune/stdio")

-- Reports what this actor's own VM state looks like, after changing it

const state = require("./state")
state.Count += 1

_G.ActorValue = "Worker"

const folder = roblox.Instance.new("Folder")
folder.Name = "WorkerFolder"

assert(folder.Parent == nil, "The synthetic scratch root must not be a parent")
assert(folder:FindFirstAncestor("ROOT") == nil, "Ancestor names must not expose the synthetic root")
assert(
	folder:FindFirstAncestorOfClass("ROOT") == nil,
	"Ancestor classes must not expose the synthetic root"
)
assert(
	folder:FindFirstAncestorWhichIsA("ROOT") == nil,
	"Ancestor inheritance must not expose the synthetic root"
)

const dataModel = roblox.Instance.new("DataModel")
const actualRoot = roblox.Instance.new("Folder")
actualRoot.Name = "ROOT"
actualRoot.Parent = dataModel
folder.Parent = actualRoot
assert(
	folder:FindFirstAncestor("ROOT") == actualRoot,
	"A real ancestor named ROOT must remain visible"
)
assert(
	folder:FindFirstAncestorOfClass("Folder") == actualRoot,
	"Real class ancestors must remain visible"
)
assert(
	folder:FindFirstAncestorWhichIsA("DataModel") == dataModel,
	"Real DataModel ancestors must remain visible"
)
assert(dataModel:FindFirstAncestor("ROOT") == nil, "A DataModel must not expose the synthetic root")
assert(
	dataModel:FindFirstAncestorOfClass("ROOT") == nil,
	"A DataModel must not expose the root class"
)
assert(
	dataModel:FindFirstAncestorWhichIsA("ROOT") == nil,
	"A DataModel must not expose root inheritance"
)
folder.Parent = nil
actualRoot:Destroy()
dataModel:Destroy()

roblox.implementMethod("Folder", "WorkerOnly", function()
	return "Worker"
end)

const promptOk, promptError = pcall(function()
	return stdio.prompt("text", "Should fail")
end)

return {
	Count = state.Count,
	GlobalValue = _G.ActorValue,
	InheritedGlobal = _G.OwnerValue,
	PromptError = if promptOk then nil else tostring(promptError),
	WorkerMethod = (folder :: any):WorkerOnly(),
	args = actor.args,
}
