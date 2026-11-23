const actor = require("@lune/actor")
const work = require("./actor_benchmark_work")

-- Runs one share of the actor benchmark inside its own actor

return work(actor.Args[1])
