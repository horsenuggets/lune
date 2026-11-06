const js = require("@lune/js")

-- The build cannot inspect these options, so `--js-include` embeds the module.
const Options = { ModuleRoot = "./Source" }
const Context = js.newContext(Options)
const Value = Context:Import("/Value.ts")
print(Value.Value)
Context:Close()
