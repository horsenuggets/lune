const actor = require("@lune/actor")

-- Replies to every "Echo" message with the same value, until the owner sends "Stop"

const parent = assert(actor.parent, "Workers should have a parent")
while true do
	const value = parent:receive("Echo")
	if value == "Stop" then
		break
	end
	parent:send("Reply", value)
end

return "Stopped"
