const js = require("@lune/js")

const Context = js.newContext()
const Dynamic = Context:Import("./Source/Dynamic.ts")
const Alias = Dynamic.Load("Alias")
print(Alias.Value)
Context:Close()
