-- Exercises value conversion, cycles, buffers, and unsupported value rejection.

const js = require("@lune/js")

const Context = js.newContext()

const Value = Context:Evaluate([[
    (() => {
        const Shared = { answer: 42 }
        const Root = {
            Array: [1, "two", true, null, undefined],
            Shared: Shared,
            Alias: Shared,
        }
        Root.Self = Root
        return Root
    })()
]])

assert(Value.Array[1] == 1)
assert(Value.Array[2] == "two")
assert(Value.Array[3] == true)
assert(Value.Array[4] == js.Null)
assert(Value.Array[5] == js.Undefined)
assert(Value.Shared == Value.Alias)
assert(Value.Self == Value)
assert(Value.Shared.answer == 42)

const Bytes = Context:Evaluate("new Uint8Array([1, 2, 255])")
assert(type(Bytes) == "buffer")
assert(buffer.readu8(Bytes, 0) == 1)
assert(buffer.readu8(Bytes, 1) == 2)
assert(buffer.readu8(Bytes, 2) == 255)

-- Arbitrary string keys stay own data properties instead of touching prototypes.
Context:Expose("prototypeKey", function()
	return { ["__proto__"] = { Injected = true }, Value = 1 }
end)
const PrototypeOwn = Context:Evaluate([[
	(() => {
		const Value = prototypeKey()
		return Object.getPrototypeOf(Value) === Object.prototype
			&& Object.prototype.hasOwnProperty.call(Value, "__proto__")
	})()
]])
assert(PrototypeOwn == true)
const Proto = Context:Evaluate("prototypeKey()")
assert(Proto.Value == 1)
assert(Proto["__proto__"].Injected == true)
assert(Proto.Injected == nil)

const Unsupported = { "1n", "Symbol('value')", "new Date()" }
for _, Source in Unsupported do
	assert(not pcall(function()
		Context:Evaluate(Source)
	end))
end

Context:Expose("sparse", function()
	return { [1] = true, [3] = true }
end)
assert(not pcall(function()
	Context:Evaluate("sparse()")
end))

Context:Close()
