const js = require("@lune/js")

const Context = js.newContext({ ModuleRoot = "./Source" })
const Path = "/Value.ts"
const Value = Context:Import(Path)
print(Value.Which)
Context:Close()
