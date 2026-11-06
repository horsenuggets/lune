const js = require("@lune/js")

-- The build cannot inspect these options, so `--js-include` embeds the modules.
const Options = { ModuleRoot = "./Source" }
const Context = js.newContext(Options)
const Entry = Context:Import("/Entry.ts")
const Extra = Entry.Load()
print(Extra.Value)
Context:Close()
