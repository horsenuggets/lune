const actor = require("@lune/actor")

-- Updates the shared table it receives as many times as requested

const SharedTable = actor.SharedTable
const shared = actor.Args[1]
const times = actor.Args[2]

for _ = 1, times do
	SharedTable.increment(shared, "Count", 1)
	SharedTable.update(shared, "Updated", function(value)
		return (value or 0) + 1
	end)
end

return SharedTable.size(shared)
