const actor = require("@lune/actor")

-- Binding only works inside actors

const bindOk, bindError = pcall(actor.bindToMessage, "Topic", function() end)
assert(not bindOk, "Binding outside of an actor should fail")
assert(string.find(tostring(bindError), "only available inside an actor", 1, true), bindError)

-- Every callback bound to a topic receives its own copy of each message

const worker = actor.spawn("./workers/bound.luau")
assert(worker:receive("Ready") == true, "Connections should start connected")

worker:send("Value", { Index = 1 })
const first = worker:receive("First")
const second = worker:receive("Second")
assert(first.Index == 1 and second.Index == 1, "Both callbacks should receive the message")
assert(first.Seen == true and second.Seen == nil, "Each callback should receive its own copy")

-- Disconnected callbacks stop receiving messages

worker:send("Disconnect", true)
assert(worker:receive("Disconnected") == false, "Disconnecting should update Connected")

worker:send("Value", { Index = 2 })
assert(worker:receive("Second").Index == 2, "Remaining callbacks should keep receiving")
worker:send("Report", true)
const counts = worker:receive("Counts")
assert(counts.First == 1 and counts.Second == 2, "Disconnected callbacks should not run")

-- Joining stops the bound callbacks, which lets the actor finish

const ok, result = worker:join()
assert(ok and result == "Bound", "Joining should let actors with bound callbacks finish")
