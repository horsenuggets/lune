const js = require("@lune/js")

const First = js.newContext({ ModuleRoot = "./A" })
const FirstValue = First:Import("/Value.ts")
print(FirstValue.Which)
First:Close()

const Second = js.newContext({ ModuleRoot = "./B" })
const SecondValue = Second:Import("/Value.ts")
print(SecondValue.Which)
Second:Close()
