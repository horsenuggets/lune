-- Exercises context creation, primitive evaluation, sentinels, and closing.

const js = require("@lune/js")

const Context = js.newContext({
	MemoryLimitBytes = 16 * 1024 * 1024,
	StackLimitBytes = 256 * 1024,
	JobBudget = 8,
})

assert(Context:Evaluate("6 * 7") == 42)
assert(Context:Evaluate("'hello ' + 'world'") == "hello world")
assert(Context:Evaluate("1 < 2") == true)
assert(Context:Evaluate("null") == js.Null)
assert(Context:Evaluate("undefined") == js.Undefined)
assert(js.Null ~= js.Undefined)

const First = js.newContext()
const Second = js.newContext()
First:Evaluate("globalThis.answer = 40")
Second:Evaluate("globalThis.answer = 2")
assert(First:Evaluate("answer") == 40)
assert(Second:Evaluate("answer") == 2)
First:Close()
Second:Close()

Context:Close()
Context:Close()

const Ok, Message = pcall(function()
	Context:Evaluate("1 + 1")
end)
assert(not Ok)
assert(string.find(tostring(Message), "JavaScript context is closed", 1, true) ~= nil)
