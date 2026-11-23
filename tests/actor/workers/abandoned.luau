const actor = require("@lune/actor")

-- Starts actors and finishes without joining them, so that its runtime must stop them

actor.spawn("./command.luau", { args = { "Idle" } })
actor.spawn("./command.luau", { args = { "Spin" } })
