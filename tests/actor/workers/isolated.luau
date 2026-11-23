const actor = require("@lune/actor")
const roblox = require("@lune/roblox")
const stdio = require("@lune/stdio")

-- Reports what this actor's own VM state looks like, after changing it

const state = require("./state")
state.Count += 1

_G.ActorValue = "Worker"

const folder = roblox.Instance.new("Folder")
folder.Name = "WorkerFolder"

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
