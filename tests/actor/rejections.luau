const actor = require("@lune/actor")
const roblox = require("@lune/roblox")

const worker = actor.spawn("./workers/echo.luau")

local function rejection(value: any): string
	const ok, message = pcall(function(): any
		worker:send("Echo", value)
		return nil
	end)
	assert(not ok, "The value should be rejected")
	return tostring(message)
end

local function expectRejection(value: any, expected: string)
	const message = rejection(value)
	assert(string.find(message, expected, 1, true), `Expected "{expected}" in "{message}"`)
end

-- Values that cannot leave their VM are rejected with clear errors

expectRejection(function() end, "Functions cannot be sent")
expectRejection(coroutine.create(function() end), "Threads cannot be sent")
expectRejection(roblox.Instance.new("Folder"), "Instances")
expectRejection({ Callback = print }, "Functions cannot be sent")
expectRejection(setmetatable({}, {}), "metatables")
expectRejection({ [{}] = true }, "table keys")

const cyclic = {}
cyclic.Self = cyclic
expectRejection(cyclic, "Cyclic tables")

-- Rejected values never reach the actor, which keeps working afterwards

worker:send("Echo", "Still working")
assert(worker:receive("Reply") == "Still working", "The actor should keep working")
worker:send("Echo", "Stop")
assert(worker:join(), "The actor should finish normally")

-- Values that an actor returns follow the same rules

const ok, message = actor.spawn("./workers/command.luau", { Args = { "ReturnFunction" } }):join()
assert(not ok, "Returning a function should fail the actor")
assert(string.find(message, "cannot be sent to its owner", 1, true), message)
