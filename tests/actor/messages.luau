const actor = require("@lune/actor")

const worker = actor.spawn("./workers/echo.luau")

local function echo(value: any): any
	worker:send("Echo", value)
	return worker:receive("Reply")
end

-- Primitive values arrive unchanged

assert(echo(true) == true, "Booleans should be sent")
assert(echo(1.5) == 1.5, "Numbers should be sent")
assert(echo("Text") == "Text", "Strings should be sent")
assert(echo(vector.create(1, 2, 3)) == vector.create(1, 2, 3), "Vectors should be sent")

-- Buffers and tables are copied

const bytes = buffer.fromstring("Bytes")
const copiedBytes = echo(bytes)
assert(buffer.tostring(copiedBytes) == "Bytes", "Buffers should be copied")
buffer.writeu8(bytes, 0, 0)
assert(buffer.tostring(copiedBytes) == "Bytes", "Buffer copies should be independent")

const source: { [any]: any } =
	{ 1, nil, 3, Name = "Value", [true] = "Key", Nested = { Deep = { 42 } } }
const copied = echo(source)
assert(copied ~= source, "Tables should be copied")
assert(copied[1] == 1 and copied[2] == nil and copied[3] == 3, "Array holes should be kept")
assert(copied.Name == "Value" and copied[true] == "Key", "Keys should be kept")
assert(copied.Nested.Deep[1] == 42, "Nested tables should be copied")
source.Nested.Deep[1] = 0
assert(copied.Nested.Deep[1] == 42, "Copies should be independent of the source")

-- Frozen tables stay frozen, and other tables do not become frozen

const frozen = echo(table.freeze({ Value = 1 }))
assert(table.isfrozen(frozen), "Frozen tables should stay frozen")
assert(not table.isfrozen(copied), "Copied tables should not become frozen")

-- Messages on each topic arrive in order, separately from other topics

for index = 1, 10 do
	worker:send("Echo", index)
end
for index = 1, 10 do
	assert(worker:receive("Reply") == index, "Messages should arrive in order")
end

-- The actor finishes once it is told to stop

worker:send("Echo", "Stop")
const ok, result = worker:join()
assert(ok and result == "Stopped", "The actor should return after stopping")

-- Joined actors no longer accept messages, and finished actors send nothing more

const sendOk, sendError = pcall(function(): any
	worker:send("Echo", 1)
	return nil
end)
assert(not sendOk, "Sending to a joined actor should fail")
assert(string.find(tostring(sendError), "no longer accepts messages", 1, true), sendError)

const receiveOk, receiveError = pcall(worker.receive, worker, "Reply")
assert(not receiveOk, "Receiving from a finished actor should fail")
assert(string.find(tostring(receiveError), "finished without sending", 1, true), receiveError)
