const actor = require("@lune/actor")
const process = require("@lune/process")

-- Starts actors and exits the process while they still run, so that its runtime must
-- stop them even though the script never reaches its end

actor.spawn("./command.luau", { args = { "Idle" } })
actor.spawn("./command.luau", { args = { "Spin" } })
process.exit(0)
