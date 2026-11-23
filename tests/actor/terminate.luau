const actor = require("@lune/actor")
const task = require("@lune/task")

local function terminated(command: string)
	const worker = actor.spawn("./workers/command.luau", { args = { command } })
	task.wait(0.05)
	worker:terminate()
	worker:terminate()
	const ok, message = worker:join()
	assert(not ok, `Terminating "{command}" should fail the actor`)
	assert(message == "The actor was terminated.", message)
	return worker
end

-- Terminating stops busy, protected and idle actors

terminated("Spin")
terminated("ProtectedSpin")
const idle = terminated("Idle")

-- Terminated actors no longer accept messages

const sendOk = pcall(idle.send, idle, "Topic", 1)
assert(not sendOk, "Terminated actors should not accept messages")

-- Terminating an actor that already finished keeps its result

const finished = actor.spawn("./workers/command.luau", { args = { "Finish" } })
assert(finished:join(), "The actor should finish")
finished:terminate()
const ok, value = finished:join()
assert(ok and value == "Finish", "Terminating a finished actor should keep its result")
