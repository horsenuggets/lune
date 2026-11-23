const actor = require("@lune/actor")
const roblox = require("@lune/roblox")

-- One actor misusing a destroyed instance must get an ordinary error, while every other
-- actor and the owner keep creating instances normally

const churning = {}
for index = 1, 3 do
	churning[index] = actor.spawn("./workers/churn.luau")
end

const ok, result = actor.spawn("./workers/destroyed_service.luau"):join()
assert(ok, result)
assert(not result.Ok, "Using a destroyed DataModel should fail")
assert(
	string.find(result.Message, "destroyed", 1, true),
	`The failure should be an ordinary destroyed-instance error, got {result.Message}`
)
assert(result.Name == "AfterDestroyedService", "The actor should keep working afterward")

for index, handle in churning do
	const churnOk, churnResult = handle:join()
	assert(churnOk, `Actor {index} should keep creating instances, got {churnResult}`)
end

const folder = roblox.Instance.new("Folder")
folder.Name = "Owner"
assert(folder.Name == "Owner", "The owner should keep creating instances")

-- The same misuse on the owner's own thread is an ordinary error too
const dataModel = roblox.Instance.new("DataModel")
dataModel:Destroy()
assert(not pcall(function()
	return dataModel:GetService("Workspace")
end), "Using a destroyed DataModel should fail on the owner too")
