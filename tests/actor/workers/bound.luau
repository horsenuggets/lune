const actor = require("@lune/actor")

-- Binds two callbacks to the same topic, reporting what each one received

const parent = assert(actor.parent, "Workers should have a parent")

local firstCount = 0
local secondCount = 0

const first = actor.bindToMessage("Value", function(value)
	firstCount += 1
	value.Seen = true
	parent:send("First", value)
end)

actor.bindToMessage("Value", function(value)
	secondCount += 1
	parent:send("Second", value)
end)

actor.bindToMessage("Disconnect", function()
	first:disconnect()
	first:disconnect()
	parent:send("Disconnected", first.connected)
end)

actor.bindToMessage("Report", function()
	parent:send("Counts", { First = firstCount, Second = secondCount })
end)

parent:send("Ready", first.connected)

-- The main thread finishes now, but the actor keeps running while callbacks are bound
return "Bound"
