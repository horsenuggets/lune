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

const Async = Context:Import("./fixtures/Figterm/Async.ts")
assert(Async.AsyncValue == 42)

const Dynamic = Context:Import("./fixtures/Figterm/Dynamic.ts")
const Extra = Dynamic.Load("Extra")
assert(Extra.Value == 42)

const Errors = Context:Import("./fixtures/Figterm/Error.ts")
const Ok, Message = pcall(Errors.ThrowExpectedError)
assert(not Ok)
assert(string.find(tostring(Message), "Error.ts", 1, true) ~= nil)

assert(pcall(function()
	Context:Import("./fixtures/Figterm/Missing.ts")
end) == false)

Context:Close()
