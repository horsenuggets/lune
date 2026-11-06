const js = require("@lune/js")

-- An `init.luau` entry anchors `ModuleRoot` to this file's own directory.
const Context = js.newContext({ ModuleRoot = "./Source" })
const Value = Context:Import("/Value.ts")
print(Value.Value)
assert(Value.Relative == 7)
Context:Close()
