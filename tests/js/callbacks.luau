-- Exercises synchronous and yielding Luau callbacks exposed to JavaScript.

const js = require("@lune/js")
const task = require("@lune/task")

const Context = js.newContext()

Context:Expose("add", function(Left: number, Right: number): number
	assert(type(Left) == "number")
	assert(type(Right) == "number")
	return Left + Right
end)

Context:Expose("pair", function(Left: string, Right: string)
	return Left, Right
end)

Context:ExposeAsync("addLater", function(Left: number, Right: number): number
	task.wait(0.01)
	return Left + Right
end)

assert(Context:Evaluate("add(20, 22)") == 42)

const Pair = Context:Evaluate("pair('first', 'second')")
assert(Pair[1] == "first")
assert(Pair[2] == "second")

assert(Context:Evaluate("addLater(20, 22)") == 42)

const Callback = Context:Evaluate([[
    (Value) => {
        return Value * 2
    }
]])
assert(Callback(21) == 42)

Context:Close()
