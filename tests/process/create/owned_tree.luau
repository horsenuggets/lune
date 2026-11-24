--[[

owned_tree

Exercises native tree cancellation while the normal status and pipe consumers keep
running. Every native fixture has an independent timeout, including blocked I/O.

--]]

const process = require("@lune/process")
const task = require("@lune/task")
const fixture = _G.OwnedProcessFixture

const function waitUntil(predicate)
	const deadline = os.clock() + 5
	while not predicate() do
		assert(os.clock() < deadline, "Owned process test timed out")
		task.wait(0.01)
	end
end

const function spawn(role, owned)
	return process.create(fixture.Executable, fixture.Args, {
		env = fixture.Environment(role),
		ownedTree = owned,
	})
end

-- Ordinary handles must reject tree termination, without killing the process.
const ordinary = spawn("unrelated", false)
waitUntil(function()
	return fixture.Read("unrelated-heartbeat") ~= nil
end)
assert(not pcall(function()
	ordinary:terminateTree()
end))

for _, invalid in { 1, "true", {} } :: { any } do
	assert(not pcall(function()
		process.create(fixture.Executable, fixture.Args, { ownedTree = invalid })
	end))
end
assert(not pcall(function()
	process.create("lune-owned-process-nonexistent-executable", {}, { ownedTree = true })
end))
assert(not pcall(function()
	process.exec(fixture.Executable, fixture.Args, { ownedTree = true })
end))

-- The same preparation must apply cwd and environment for both backends.
for _, owned in { false, true } do
	const configured = process.create(fixture.Executable, fixture.Args, {
		cwd = fixture.Directory,
		env = fixture.Environment("configuration"),
		ownedTree = owned,
	})
	const configuredStatus = configured:status()
	const configuredOutput = configured.stdout:readToEnd()
	const configuredError = configured.stderr:readToEnd()
	assert(configuredStatus.ok, configuredOutput .. configuredError)
	if owned then
		configured:terminateTree()
	end
end

const shellChild = process.create("exit", { "17" }, { shell = true, ownedTree = true })
assert(shellChild:status().code == 17)
shellChild:terminateTree()
shellChild.stdout:readToEnd()
shellChild.stderr:readToEnd()

for _, role in { "direct", "launcher", "early", "output", "blocked-stdin" } do
	const previousHeartbeat = fixture.Read("grandchild-heartbeat")
	const child = spawn(role, true)
	waitUntil(function()
		if role == "launcher" or role == "early" then
			const heartbeat = fixture.Read("grandchild-heartbeat")
			return heartbeat ~= nil and heartbeat ~= previousHeartbeat
		end
		return fixture.Read(`{role}-ready`) ~= nil
	end)
	if role == "early" then
		-- Ownership must survive observing and caching the launcher's successful exit.
		assert(child:status().ok)
		assert(child:status().code == 0)
	end
	if role == "output" then
		-- Let both native pipes saturate before starting the normal consumers.
		task.wait(0.1)
	end

	local statusCount = 0
	local stdout = nil
	local stderr = nil
	local writerFinished = role ~= "blocked-stdin"
	local closeFinished = role ~= "blocked-stdin"
	local writerSucceeded = false
	local firstCode = nil
	if role ~= "early" then
		const canceledStatus = task.spawn(function()
			child:status()
		end)
		task.cancel(canceledStatus)
	end
	for _ = 1, 3 do
		task.spawn(function()
			const status = child:status()
			if firstCode == nil then
				firstCode = status.code
			end
			assert(firstCode == status.code)
			statusCount += 1
		end)
	end
	task.spawn(function()
		stdout = child.stdout:readToEnd()
	end)
	task.spawn(function()
		stderr = child.stderr:readToEnd()
	end)
	if role == "blocked-stdin" then
		task.spawn(function()
			writerSucceeded = pcall(function()
				child.stdin:write(string.rep("I", 4 * 1024 * 1024))
			end)
			writerFinished = true
		end)
		task.wait(0.05)
		assert(not writerFinished, "The fixture must block the native stdin writer")
		task.spawn(function()
			child.stdin:close()
			closeFinished = true
		end)
	end

	-- Model the required consumer on cancellation or a monitor failure. A canceled
	-- termination waiter must not cancel cleanup or consume somebody else's result.
	const monitorOk = pcall(function()
		error("controlled monitor failure")
	end)
	assert(not monitorOk)
	const canceled = task.spawn(function()
		child:terminateTree()
	end)
	task.cancel(canceled)
	if role == "direct" then
		-- No second termination call may be needed to finish the first cleanup.
		waitUntil(function()
			return statusCount == 3 and stdout ~= nil and stderr ~= nil
		end)
	end
	local terminatedCount = 0
	for _ = 1, 3 do
		task.spawn(function()
			child:terminateTree()
			terminatedCount += 1
		end)
	end
	waitUntil(function()
		return terminatedCount == 3
			and statusCount == 3
			and stdout ~= nil
			and stderr ~= nil
			and writerFinished
			and closeFinished
	end)
	child:terminateTree()
	assert(child:status().code == firstCode)
	assert(not pcall(function()
		child.stdin:write("closed")
	end))
	child.stdin:close()
	if role == "blocked-stdin" then
		assert(not writerSucceeded)
	end
	if role == "output" then
		assert(string.find(stdout, string.rep("O", 4096), 1, true))
		assert(string.find(stderr, string.rep("E", 4096), 1, true))
	end
	if role == "launcher" or role == "early" then
		const heartbeat = fixture.Read("grandchild-heartbeat")
		task.wait(0.15)
		assert(
			fixture.Read("grandchild-heartbeat") == heartbeat,
			"Grandchild survived tree termination"
		)
	end
end

-- Direct kill remains direct even on an owned handle, and does not lose ownership.
const beforeKill = fixture.Read("grandchild-heartbeat")
const killed = spawn("launcher", true)
waitUntil(function()
	return fixture.Read("grandchild-heartbeat") ~= beforeKill
end)
killed:kill()
assert(not killed:status().ok)
const afterKill = fixture.Read("grandchild-heartbeat")
waitUntil(function()
	return fixture.Read("grandchild-heartbeat") ~= afterKill
end)
killed:terminateTree()
killed.stdout:readToEnd()
killed.stderr:readToEnd()

-- Handle collection must clean up even if its launcher has already exited.
const beforeDrop = fixture.Read("grandchild-heartbeat")
local abandoned: process.ChildProcess? = spawn("early", true)
assert(abandoned ~= nil)
assert(abandoned:status().ok)
waitUntil(function()
	return fixture.Read("grandchild-heartbeat") ~= beforeDrop
end)
abandoned = nil
fixture.Collect()
task.wait(0.1)
const afterDrop = fixture.Read("grandchild-heartbeat")
task.wait(0.15)
assert(fixture.Read("grandchild-heartbeat") == afterDrop)

const unrelatedBefore = fixture.Read("unrelated-heartbeat")
waitUntil(function()
	return fixture.Read("unrelated-heartbeat") ~= unrelatedBefore
end)
ordinary:kill()
ordinary:status()
ordinary.stdout:readToEnd()
ordinary.stderr:readToEnd()
