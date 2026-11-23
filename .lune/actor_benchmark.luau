const actor = require("@lune/actor")
const datetime = require("@lune/datetime")
const process = require("@lune/process")
const work = require("./actor_benchmark_work")

-- Compares running the same CPU-bound work serially and in parallel actors
--
-- [Usage] lune run actor_benchmark [workers] [iterations]

const workers = tonumber(process.args[1]) or 4
const iterations = tonumber(process.args[2]) or 20_000_000

local function now(): number
	return datetime.now().unixTimestampMillis / 1000
end

const serialStarted = now()
local serialTotal = 0
for _ = 1, workers do
	serialTotal += work(iterations)
end
const serialSeconds = now() - serialStarted

const parallelStarted = now()
const handles = {}
for index = 1, workers do
	handles[index] = actor.spawn("./actor_benchmark_worker.luau", { Args = { iterations } })
end
local parallelTotal = 0
for _, handle in handles do
	const ok, total = handle:join()
	assert(ok, total)
	parallelTotal += total
end
const parallelSeconds = now() - parallelStarted

assert(serialTotal == parallelTotal, "Both halves should compute the same result")

const units = if workers == 1 then "actor" else "actors"
print(`Ran {workers} units of {iterations} iterations each.`)
print(`Serial execution took {string.format("%.3f", serialSeconds)} seconds.`)
print(`Parallel execution in {workers} {units} took {string.format("%.3f", parallelSeconds)} seconds.`)
print(`The speedup was {string.format("%.2f", serialSeconds / parallelSeconds)} times.`)
