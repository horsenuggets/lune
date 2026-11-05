const js = require("@lune/js")

const Context = js.newContext({ ModuleRoot = ".." })
const Value = Context:Import("./Value.ts")
print(Value.Which)
Context:Close()
