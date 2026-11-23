const actor = require("@lune/actor")

local function run(...: any): (boolean, any)
	const worker = actor.spawn("./workers/command.luau", { args = { ... } })
	return worker:join()
end

-- Errors fail the actor with their message and traceback

const errorOk, errorMessage = run("Error")
assert(not errorOk, "Errors should fail the actor")
assert(string.find(errorMessage, "Controlled actor error", 1, true), errorMessage)
assert(string.find(errorMessage, "command", 1, true), "Errors should name the failing script")
assert(string.find(errorMessage, "Line", 1, true), "Errors should include a traceback")

-- Errors from other threads also fail the actor

const spawnedOk, spawnedMessage = run("SpawnedError")
assert(not spawnedOk, "Errors from spawned threads should fail the actor")
assert(string.find(spawnedMessage, "Controlled spawned error", 1, true), spawnedMessage)

-- Exiting ends only the actor, succeeding only with code zero

const exitOk, exitMessage = run("Exit", 3)
assert(not exitOk, "Exiting with a nonzero code should fail the actor")
assert(string.find(exitMessage, "exited with code 3", 1, true), exitMessage)

const zeroOk, zeroValue = run("Exit", 0)
assert(zeroOk and zeroValue == nil, "Exiting with code zero should succeed without values")

-- The owner keeps running after its actors fail or exit

const finished, value = run("Finish")
assert(finished and value == "Finish", "Later actors should still work")
