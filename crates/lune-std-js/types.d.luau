--[=[
	@interface JsContextOptions
	@within Js

	Options for a JavaScript context.

	* `ModuleRoot` - Optional root directory for host JavaScript imports. Relative paths resolve from the calling Luau file's directory. Standalone binaries always use their embedded modules.
	* `MemoryLimitBytes` - Maximum QuickJS heap size in bytes. Zero removes the limit.
	* `StackLimitBytes` - Maximum QuickJS stack size in bytes.
	* `ExecutionLimitMs` - Maximum JavaScript execution time per operation in milliseconds.
	* `OperationTimeoutMs` - Wall-clock timeout for an entire operation, including asynchronous waits.
	* `JobBudget` - Number of pending JavaScript jobs to run before yielding to the Luau scheduler.
]=]
export type JsContextOptions = {
	ModuleRoot: string?,
	MemoryLimitBytes: number?,
	StackLimitBytes: number?,
	ExecutionLimitMs: number?,
	OperationTimeoutMs: number?,
	JobBudget: number?,
}

--[=[
	@interface JsEvaluateOptions
	@within Js

	Options for one JavaScript evaluation.

	* `Filename` - Filename used in JavaScript exceptions and stack traces.
	* `Mode` - Either `"script"` or `"module"`. Module mode supports top-level await.
]=]
export type JsEvaluateOptions = {
	Filename: string?,
	Mode: ("script" | "module")?,
}

-- Structural brands keep the userdata sentinels distinct in Luau's type system.
export type JsNull = { __jsNull: "JsNull" }
export type JsUndefined = { __jsUndefined: "JsUndefined" }
export type JsFunction = (...any) -> any
export type JsValue = any
export type JsCallback = (...any) -> ...any

--[=[
	@interface JsContext
	@within Js

	An isolated QuickJS runtime and global context. Only one operation may be active at a time.
]=]
local JsContext = {}

--[=[
	@within JsContext

	Evaluates JavaScript source and awaits a returned Promise or thenable. Primitive values,
	plain objects, arrays, buffers, and JavaScript functions are converted to Luau. JavaScript
	`null` and `undefined` become `js.Null` and `js.Undefined` respectively. Unsupported values,
	exceptions, and Promise rejections raise an error.

	@param source The JavaScript source to evaluate
	@param options Optional filename and evaluation mode
]=]
function JsContext.Evaluate(
	self: JsContext,
	source: string,
	options: JsEvaluateOptions?
): JsValue
	return nil :: any
end

--[=[
	@within JsContext

	Imports a JavaScript or TypeScript ES module and awaits its namespace, including top-level await.
	Relative specifiers resolve from the calling Luau file. Host imports are confined to ModuleRoot,
	and standalone imports use only modules embedded by `lune build`.

	@param specifier The module specifier to import
]=]
function JsContext.Import(self: JsContext, specifier: string): { [string]: JsValue }
	return nil :: any
end

--[=[
	@within JsContext

	Exposes a synchronous Luau callback as a global JavaScript function. Arguments and return
	values use the same conversions as `Evaluate`. Zero callback returns become `undefined`, one
	return is used directly, and multiple returns become a JavaScript array. The callback must
	not yield.

	@param name The global JavaScript function name
	@param callback The synchronous Luau callback
]=]
function JsContext.Expose(self: JsContext, name: string, callback: JsCallback): () end

--[=[
	@within JsContext

	Exposes a Luau callback that may yield as a global JavaScript function. JavaScript receives a
	Promise for the callback result.

	@param name The global JavaScript function name
	@param callback The yielding Luau callback
]=]
function JsContext.ExposeAsync(self: JsContext, name: string, callback: JsCallback): () end

--[=[
	@within JsContext

	Closes the runtime, interrupts active JavaScript execution, and wakes pending operations.
	Further evaluation and exported function calls raise an error. Calling Close again is safe.
]=]
function JsContext.Close(self: JsContext): () end

export type JsContext = typeof(JsContext)

--[=[
	@class Js

	JavaScript evaluation using QuickJS.

	```luau
	local js = require("@lune/js")
	local context = js.newContext()
	print(context:Evaluate("Promise.resolve(40 + 2)"))
	context:Close()
	```
]=]
local js = {}

js.Null = (nil :: any) :: JsNull
js.Undefined = (nil :: any) :: JsUndefined

--[=[
	@within Js

	Creates an isolated JavaScript context.

	@param options Resource limits, operation limits, and scheduling options for the runtime
]=]
function js.newContext(options: JsContextOptions?): JsContext
	return nil :: any
end

return js
