-- Exercises JavaScript exceptions, Promise rejections, and operation errors.

const js = require("@lune/js")

const Context = js.newContext({ ExecutionLimitMs = 5, OperationTimeoutMs = 20 })

const ThrowOk, ThrowMessage = pcall(function()
	Context:Evaluate("throw new Error('expected throw')", { Filename = "named-script.js" })
end)
assert(not ThrowOk)
const ThrowText = tostring(ThrowMessage)
assert(string.find(ThrowText, "expected throw", 1, true) ~= nil)
assert(string.find(ThrowText, "named-script.js", 1, true) ~= nil)

const RejectOk, RejectMessage = pcall(function()
	Context:Evaluate("Promise.reject(new Error('expected rejection'))")
end)
assert(not RejectOk)
assert(string.find(tostring(RejectMessage), "expected rejection", 1, true) ~= nil)

const LimitOk, LimitMessage = pcall(function()
	Context:Evaluate("while (true) {}")
end)
assert(not LimitOk)
assert(string.find(tostring(LimitMessage), "execution limit exceeded", 1, true) ~= nil)

const TimeoutOk, TimeoutMessage = pcall(function()
	Context:Evaluate("new Promise(() => {})")
end)
assert(not TimeoutOk)
assert(string.find(tostring(TimeoutMessage), "operation timed out", 1, true) ~= nil)

assert(Context:Evaluate("40 + 2") == 42)

Context:Close()
