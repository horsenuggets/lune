const actor = require("@lune/actor")

-- The main runtime is not an actor

assert(actor.isWorker == false, "The main runtime should not be an actor")
assert(actor.parent == nil, "The main runtime should not have a parent")
assert(#actor.args == 0, "The main runtime should have empty actor arguments")

-- Actors receive copies of their arguments and return values to their owner

const arguments = { 1, 2, 3 }
const worker = actor.spawn("./workers/sum.luau", { args = arguments })
arguments[1] = 100
const ok, total, context = worker:join()
assert(ok, total)
assert(total == 6, `Actors should receive copied arguments, got "{total}"`)
assert(context.IsWorker == true, "Actors should know that they are actors")
assert(context.HasParent == true, "Actors should have a parent")

-- Joining again returns the same result

const again, totalAgain = worker:join()
assert(again and totalAgain == 6, "Joining twice should return the same result")

-- Actors without options receive empty arguments

const empty, emptyTotal = actor.spawn("./workers/sum.luau"):join()
assert(empty and emptyTotal == 0, "Actors without arguments should receive an empty table")

-- Paths that do not start with a dot are relative to the working directory

const fromCwd, cwdTotal = actor.spawn("tests/actor/workers/sum.luau", { args = { 2 } }):join()
assert(fromCwd and cwdTotal == 2, "Paths without a leading dot should use the working directory")

-- Invalid options are rejected before any actor starts

const unknownOk, unknownError = pcall(actor.spawn, "./workers/sum.luau", { Unknown = true } :: any)
assert(not unknownOk, "Unknown options should be rejected")
assert(string.find(tostring(unknownError), "Unknown actor option", 1, true), unknownError)

const argsOk, argsError = pcall(actor.spawn, "./workers/sum.luau", { args = 5 } :: any)
assert(not argsOk, "Arguments that are not tables should be rejected")
assert(string.find(tostring(argsError), "must be a table", 1, true), argsError)

-- Missing scripts fail through join instead of erroring while spawning

const missing, missingError = actor.spawn("./workers/does_not_exist.luau"):join()
assert(not missing, "Missing scripts should fail")
assert(string.find(tostring(missingError), "does_not_exist", 1, true), missingError)

-- Actors can start their own actors

const nested, nestedTotal = actor.spawn("./workers/nested.luau"):join()
assert(nested, nestedTotal)
assert(nestedTotal == 10, `Nested actors should return their results, got "{nestedTotal}"`)

-- Actors are userdata with their own type

const kind: string = typeof(worker)
assert(kind == "Actor", `Actors should have the "Actor" type, got "{kind}"`)
