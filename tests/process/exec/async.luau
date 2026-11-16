local process = require("@lune/process")
local task = require("@lune/task")

local fixture = _G.ProcessExecFixture
local finished = false
local result = nil
local succeeded = false

task.spawn(function()
	succeeded, result =
		pcall(process.exec, fixture.Executable, fixture.Args, { env = fixture.Environment })
	finished = true
end)

-- The native child publishes readiness before waiting for this Luau thread to release it.
-- Blocking process execution cannot service this handshake and the child fails its gate.
while not fixture.IsReady() and not finished do
	task.wait()
end
assert(not finished, "The child process exited before the Luau scheduler released it")

local sleepStart = os.clock()
fixture.Release()
while not finished do
	task.wait()
end

assert(succeeded, tostring(result))
assert(result.ok, result.stderr)
assert(
	(os.clock() - sleepStart) >= 0.25,
	"Spawning a process that does blocking sleep did not sleep enough"
)
