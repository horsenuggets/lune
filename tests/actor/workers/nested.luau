const actor = require("@lune/actor")

-- Starts its own actor, so that actors can own other actors

const child = actor.spawn("./sum.luau", { args = { 4, 5 } })
const ok, total = child:join()
assert(ok, total)

return total + 1
