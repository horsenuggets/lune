const js = require("@lune/js")

return function()
    const Context = js.newContext()
    const Value = Context:Import("./Value.ts")
    assert(Value.Which == "worker")
    print(Value.Which)
    Context:Close()
end
