--[=[
	@class Actor

	Runs scripts in parallel on other OS threads, similar to Roblox's Parallel Luau.

	Every actor runs its script in its own Luau VM, with its own `require` cache, globals,
	coverage data and Roblox documents. Values sent between actors are copied, except for
	`SharedTable` values, which every actor can update at the same time.

	### Example usage

	```lua
	-- Main.luau
	local actor = require("@lune/actor")

	local worker = actor.spawn("./Worker.luau", { Args = { 10 } })
	worker:send("Greeting", { Text = "Hello" })
	print(worker:receive("Progress")) --> Started

	local ok, result = worker:join()
	print(ok, result) --> true, 55

	-- Worker.luau
	local actor = require("@lune/actor")

	local parent = assert(actor.parent)
	print(parent:receive("Greeting").Text) --> Hello
	parent:send("Progress", "Started")

	local total = 0
	for index = 1, actor.Args[1] do
		total += index
	end
	return total
	```

	### Values

	Messages, arguments and returned values may contain `nil`, booleans, numbers,
	vectors, strings, buffers, tables without metatables and `SharedTable` values.
	Tables are copied deeply and keep their frozen state, while cycles, functions,
	threads and other userdata like Instances are rejected with an error.

	### Lifetime

	`join` waits for an actor to finish and stops sending it messages, which also ends any
	`bindToMessage` callbacks. When the owning script finishes, every actor it started
	that is still running is terminated. Terminating an actor makes its running Luau code
	error at the next interrupt check, so blocking native calls finish first.
]=]
const actor = {}

--[=[
	@within Actor
	@interface Actor
	.send (self: Actor, topic: string, value: any) -> () -- Copies a value to the actor under a topic
	.receive (self: Actor, topic: string) -> any -- Yields until the actor sends a value under a topic
	.join (self: Actor) -> (boolean, ...any) -- Yields until the actor finishes, returning its values or error
	.terminate (self: Actor) -> () -- Stops the actor as soon as possible
]=]
export type Actor = {
	join: (self: Actor) -> (boolean, ...any),
	receive: (self: Actor, topic: string) -> any,
	send: (self: Actor, topic: string, value: any) -> (),
	terminate: (self: Actor) -> (),
}

--[=[
	@within Actor
	@interface ActorParent
	.send (self: ActorParent, topic: string, value: any) -> () -- Copies a value to the owner under a topic
	.receive (self: ActorParent, topic: string) -> any -- Yields until the owner sends a value under a topic
]=]
export type ActorParent = {
	receive: (self: ActorParent, topic: string) -> any,
	send: (self: ActorParent, topic: string, value: any) -> (),
}

--[=[
	@within Actor
	@interface ActorConnection
	.Connected boolean -- Whether the callback still receives messages
	.Disconnect (self: ActorConnection) -> () -- Stops the callback from receiving messages
]=]
export type ActorConnection = {
	Connected: boolean,
	Disconnect: (self: ActorConnection) -> (),
}

--[=[
	@within Actor
	@interface SpawnOptions
	.Args { any }? -- Values copied into `actor.Args` inside the new actor
]=]
export type SpawnOptions = {
	Args: { any }?,
}

--[=[
	@within Actor

	A table that every actor can read and update atomically, without copying it.

	Keys must be strings or finite numbers. Values may be booleans, numbers, vectors,
	strings or other shared tables, and plain tables become new shared tables.
]=]
export type SharedTable = { [string | number]: any }

--[=[
	@within Actor
	@interface SharedTableLibrary
	.new (value: { [string | number]: any }?) -> SharedTable -- Creates a shared table
	.clear (table: SharedTable) -> () -- Removes every entry
	.clone (table: SharedTable, deep: boolean?) -> SharedTable -- Copies the entries into a new shared table
	.cloneAndFreeze (table: SharedTable, deep: boolean?) -> SharedTable -- Copies the entries into a frozen shared table
	.increment (table: SharedTable, key: string | number, delta: number) -> number -- Atomically adds to a number, returning the previous value
	.isFrozen (table: SharedTable) -> boolean -- Returns whether the shared table is frozen
	.size (table: SharedTable) -> number -- Returns the number of entries
	.update (table: SharedTable, key: string | number, callback: (any) -> any) -> () -- Atomically replaces a value with the callback's result
]=]
export type SharedTableLibrary = {
	new: (value: { [string | number]: any }?) -> SharedTable,
	clear: (table: SharedTable) -> (),
	clone: (table: SharedTable, deep: boolean?) -> SharedTable,
	cloneAndFreeze: (table: SharedTable, deep: boolean?) -> SharedTable,
	increment: (table: SharedTable, key: string | number, delta: number) -> number,
	isFrozen: (table: SharedTable) -> boolean,
	size: (table: SharedTable) -> number,
	update: (table: SharedTable, key: string | number, callback: (any) -> any) -> (),
}

--[=[
	@within Actor
	@prop Args { any }

	The arguments the owner passed to `actor.spawn`, or an empty table outside of actors.
]=]
actor.Args = {} :: { any }

--[=[
	@within Actor
	@prop isWorker boolean

	Whether the current script runs inside an actor.
]=]
actor.isWorker = false :: boolean

--[=[
	@within Actor
	@prop parent ActorParent?

	The handle used to message the owner of this actor, or `nil` outside of actors.
]=]
actor.parent = nil :: ActorParent?

--[=[
	@within Actor
	@prop SharedTable SharedTableLibrary

	Functions for creating and atomically updating shared tables.
]=]
actor.SharedTable = {} :: SharedTableLibrary

--[=[
	@within Actor

	Starts a script in a new actor, running on its own OS thread with its own Luau VM.

	Paths starting with `./` or `../` are relative to the calling script, and other paths
	are relative to the current working directory.

	@param path The path to the script to run
	@param options Options for the new actor
	@return The handle used to message, join or terminate the actor
]=]
function actor.spawn(path: string, options: SpawnOptions?): Actor
	return nil :: any
end

--[=[
	@within Actor

	Runs a callback in a new thread for every message the owner sends under a topic, like
	`Actor:BindToMessage` in Roblox. Only available inside an actor.

	Every callback bound to a topic receives its own copy of each message. Avoid also
	calling `actor.parent:receive` for a topic that has bound callbacks, since only one of
	them would get each message.

	@param topic The topic to listen to
	@param callback The function to call with each message
	@return A connection that can stop the callback
]=]
function actor.bindToMessage(topic: string, callback: (value: any) -> ()): ActorConnection
	return nil :: any
end

return actor
