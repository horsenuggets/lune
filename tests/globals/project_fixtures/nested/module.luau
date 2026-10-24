-- Lives in a nested repository while the project test runs, and yields during require so
-- its path stays on the require stack while other threads read their own project

const task = require("@lune/task")

task.wait(0.5)

return tostring(project)
