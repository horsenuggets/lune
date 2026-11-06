const js = require("@lune/js")

return function()
    const Context = js.newContext({ ModuleRoot = "./Source" })
    const Value = Context:Import("/Value.ts")
    print(Value.Which)
    Context:Close()
end
