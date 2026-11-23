const actor = require("@lune/actor")

const SharedTable = actor.SharedTable

-- Shared tables behave like tables with atomic helpers

const shared = SharedTable.new({ Count = 0, Name = "Shared", Nested = { Value = 1 } })
const kind: string = typeof(shared)
assert(kind == "SharedTable", `Expected "SharedTable", got "{kind}"`)
assert(shared.Name == "Shared", "Values should be readable")
const nestedKind: string = typeof(shared.Nested)
assert(nestedKind == "SharedTable", "Nested tables should become shared tables")
assert(SharedTable.size(shared) == 3, "Size should count entries")

shared.Name = nil
assert(shared.Name == nil and SharedTable.size(shared) == 2, "Assigning nil should remove entries")

assert(SharedTable.increment(shared, "Count", 5) == 0, "Increment should return the previous value")
assert(shared.Count == 5, "Increment should add to the value")

SharedTable.update(shared, "Count", function(value)
	return value * 2
end)
assert(shared.Count == 10, "Update should replace the value")

const keys = {}
for key, value in shared do
	keys[key] = value
end
assert(keys.Count == 10 and keys.Nested == shared.Nested, "Iteration should visit every entry")

-- Clones copy entries, sharing nested tables unless deep

const shallow = SharedTable.clone(shared)
const deep = SharedTable.clone(shared, true)
shared.Nested.Value = 2
assert(shallow.Nested.Value == 2, "Shallow clones should share nested tables")
assert(deep.Nested.Value == 1, "Deep clones should copy nested tables")
assert(shallow ~= shared and shared == shared, "Clones should be distinct shared tables")

-- Frozen clones reject every write

const frozen = SharedTable.cloneAndFreeze(shared, true)
assert(
	SharedTable.isFrozen(frozen) and not SharedTable.isFrozen(shared),
	"Only clones should freeze"
)
assert(not pcall(function()
	frozen.Count = 1
end), "Frozen tables should reject assignment")
assert(
	not pcall(SharedTable.increment, frozen, "Count", 1),
	"Frozen tables should reject increments"
)
assert(not pcall(SharedTable.clear, frozen), "Frozen tables should reject clearing")
assert(not pcall(SharedTable.cloneAndFreeze, shared), "Shallow freezing needs frozen nested tables")

-- Invalid keys and values are rejected

assert(not pcall(function()
	(shared :: any)[true] = 1
end), "Boolean keys should be rejected")
assert(not pcall(function()
	shared.Callback = print
end), "Functions should be rejected")
assert(not pcall(SharedTable.increment, shared, "Missing", 1), "Increment needs a number")

SharedTable.clear(deep)
assert(SharedTable.size(deep) == 0, "Clear should remove every entry")

-- Every actor updates the same table atomically

const counter = SharedTable.new({ Count = 0 })
const workers = {}
for index = 1, 4 do
	workers[index] = actor.spawn("./workers/counter.luau", { args = { counter :: any, 500 } })
end
for _, worker in workers do
	const ok, size = worker:join()
	assert(ok, size)
end
assert(counter.Count == 2000, `Increments should be atomic, got "{counter.Count}"`)
assert(counter.Updated == 2000, `Updates should be atomic, got "{counter.Updated}"`)
