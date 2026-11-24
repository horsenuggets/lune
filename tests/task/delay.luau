local task = require("@lune/task")

-- Delaying a task should return the thread that can then be cancelled

local thread = task.delay(0, function() end)
assert(type(thread) == "thread", "Delay should return the thread spawned")

-- Delayed functions should never run right away

local flag: boolean = false
task.delay(0, function()
	flag = true
end)
assert(not flag, "Delay should not run instantly or block")
task.wait(0.05)
assert(flag, "Delay should run after the wanted duration")

-- Delayed functions should work with yielding

local started: boolean = false
local finished: boolean = false
task.delay(0.05, function()
	started = true
	task.wait(0.1)
	finished = true
end)
local yieldDeadline = os.clock() + 5
while not finished and os.clock() < yieldDeadline do
	task.wait()
end
assert(started, "Delay should work with yielding (1)")
assert(finished, "Delay should work with yielding (2)")

-- Defer should be able to be nested

local flag4: boolean = false
task.defer(function()
	local function nested3()
		task.defer(function()
			task.wait(0.05)
			flag4 = true
		end)
	end
	local function nested2()
		task.defer(function()
			task.wait(0.05)
			nested3()
		end)
	end
	local function nested1()
		task.defer(function()
			task.wait(0.05)
			nested2()
		end)
	end
	task.wait(0.05)
	nested1()
end)
local nestedDeadline = os.clock() + 5
while not flag4 and os.clock() < nestedDeadline do
	task.wait()
end
assert(flag4, "Defer should work with nesting")

-- Varargs should get passed correctly

local fcheck = require("./fcheck")

local function f(...: any)
	fcheck(1, "string", select(1, ...))
	fcheck(2, "number", select(2, ...))
	fcheck(3, "function", select(3, ...))
end

task.delay(0, f, "", 1, f)
task.delay(0, f, "inf", math.huge, f)
task.delay(0, f, "NaN", 0 / 0, f)
