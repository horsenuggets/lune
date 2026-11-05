const js = require("@lune/js")

const Context = js.newContext({ ModuleRoot = "./Source" })

Context:ExposeAsync("HostFormatStory", function(Name: string, Projects: { string }): string
    assert(type(Name) == "string")
    assert(type(Projects) == "table")
    return `{Name} → {table.concat(Projects, ", ")}`
end)

const Catalog = Context:Import("/Catalog.ts")
const Stories = Catalog.CreateCatalog("Rooted", {
    {
        Name = "Default",
        Projects = { "XSH" },
        Terminal = { Columns = 80, Rows = 24 },
    },
})

assert(#Stories == 1)
print(Catalog.RenderStory(Stories[1]))

const Dynamic = Context:Import("/Dynamic.ts")
const Extra = Dynamic.Load("Extra")
assert(Extra.Value == 42)

const Absolute = Context:Import("/Absolute.ts")
assert(Absolute.AbsoluteValue == 42)

Context:Close()
