const actor = require("@lune/actor")

-- Returns the sum of its arguments together with details about its own context

local total = 0
for _, value in actor.args do
	total += value
end

return total, {
	HasParent = actor.parent ~= nil,
	IsWorker = actor.isWorker,
}
