-- Exercises TypeScript imports, cycles, top-level await, dynamic imports, and errors.

const js = require("@lune/js")

const Context = js.newContext()

const Catalog = Context:Import("./fixtures/Figterm/Catalog.ts")
const Stories = Catalog.CreateCatalog("ProjectList", {
	{ Name = "Default", Projects = { "XSH", "SSH CFG" } },
	{ Name = "Empty", Projects = {} },
})

assert(#Stories == 2)
assert(Stories[1].Name == "ProjectList.Default")
assert(Stories[1].Projects[1] == "XSH")
assert(#Stories[2].Projects == 0)
assert(Catalog.CountProjects(Stories[1]) == 2)

const Cycle = Context:Import("./fixtures/Figterm/CycleA.ts")
assert(Cycle.ReadCycle() == "AB")

-- Same-named modules in different directories must not share an identity.
const TopValue = Context:Import("./fixtures/Figterm/Value.ts")
const Consumer = require("./fixtures/Figterm/Collision/Consumer")
const NestedValue = Consumer(Context)
assert(TopValue.Which == "top")
assert(NestedValue.Which == "nested")

-- Callers outside ModuleRoot may still import absolute modules inside it.
const Outside = require("../js_outside/Outside")
const OutsideCatalog = Outside(Context)
assert(OutsideCatalog.CountProjects({ Name = "Outside", Projects = { "One" } }) == 1)

-- Dynamic imports resolve lazily, so an unused missing target does not fail the import.
const Optional = Context:Import("./fixtures/Figterm/Optional.ts")
assert(Optional.Marker == 1)
assert(pcall(Optional.Load) == false)

const Async = Context:Import("./fixtures/Figterm/Async.ts")
assert(Async.AsyncValue == 42)

const Dynamic = Context:Import("./fixtures/Figterm/Dynamic.ts")
const Extra = Dynamic.Load("Extra")
assert(Extra.Value == 42)

const Errors = Context:Import("./fixtures/Figterm/Error.ts")
const Ok, Message = pcall(Errors.ThrowExpectedError)
assert(not Ok)
assert(string.find(tostring(Message), "Error.ts:4:", 1, true) ~= nil)

assert(pcall(function()
	Context:Import("./fixtures/Figterm/Missing.ts")
end) == false)

Context:Close()
