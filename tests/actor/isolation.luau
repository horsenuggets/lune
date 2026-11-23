const actor = require("@lune/actor")
const roblox = require("@lune/roblox")

-- Each actor runs in its own VM, with its own modules, globals and Roblox state

const state = require("./workers/state")
state.Count = 100
_G.OwnerValue = "Owner"

const results = {}
for index = 1, 2 do
	const ok, result = actor.spawn("./workers/isolated.luau", { Args = { index } }):join()
	assert(ok, result)
	results[index] = result
end

for index, result in results do
	assert(result.Count == 1, "Each actor should require its own module copies")
	assert(result.GlobalValue == "Worker", "Each actor should have its own globals")
	assert(result.InheritedGlobal == nil, "Actors should not see their owner's globals")
	assert(result.WorkerMethod == "Worker", "Actors should keep their own method implementations")
	assert(result.Args[1] == index, "Each actor should receive its own arguments")
	assert(
		result.PromptError
			and string.find(result.PromptError, "unavailable inside an actor", 1, true),
		"Actors should not be able to prompt"
	)
end

assert(state.Count == 100, "Actors should not change the owner's modules")
assert(_G.ActorValue == nil, "Actors should not change the owner's globals")

const folder = roblox.Instance.new("Folder")
assert(not pcall(function()
	return (folder :: any):WorkerOnly()
end), "Method implementations from actors should not reach the owner")
