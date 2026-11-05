const js = require("@lune/js")

const Context = js.newContext()

Context:ExposeAsync("HostFormatStory", function(Name: string, Projects: { string }): string
    assert(type(Name) == "string")
    assert(type(Projects) == "table")
    const Summary = if #Projects == 0 then "No projects" else table.concat(Projects, ", ")
    return `{Name} → {Summary}`
end)

const Catalog = Context:Import("./Source/Catalog.ts")
const Stories = Catalog.CreateCatalog("ProjectList", {
    {
        Name = "Default",
        Projects = { "XSH", "SSH CFG" },
        Terminal = { Columns = 80, Rows = 24 },
    },
    {
        Name = "Empty",
        Projects = {},
        Terminal = { Columns = 40, Rows = 12 },
    },
})

assert(#Stories == 2)
for _, Story in Stories do
    print(Catalog.RenderStory(Story))
end

const Async = Context:Import("./Source/Async.ts")
assert(Async.AsyncValue == 42)

const Cycle = Context:Import("./Source/CycleA.ts")
assert(Cycle.ReadCycle() == "AB")

const Dynamic = Context:Import("./Source/Dynamic.ts")
const Extra = Dynamic.Load("Extra")
assert(Extra.Value == 42)

const Errors = Context:Import("./Source/Error.ts")
const ErrorOk, ErrorMessage = pcall(function()
    Errors.ThrowExpectedError()
end)
assert(not ErrorOk)
assert(string.find(tostring(ErrorMessage), "Error.ts:4:", 1, true))

const Reject = Context:Import("./Source/Reject.ts")
const RejectOk, RejectMessage = pcall(function()
    Reject.RejectExpectedError()
end)
assert(not RejectOk)
assert(string.find(tostring(RejectMessage), "Expected standalone rejection", 1, true))

Context:Close()
