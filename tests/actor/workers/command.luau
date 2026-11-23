const actor = require("@lune/actor")
const process = require("@lune/process")
const task = require("@lune/task")

-- Behaves according to the command in its first argument, so that tests can exercise
-- failures, exits and termination without needing a separate script for each one

const command = actor.args[1]

if command == "Error" then
	error("Controlled actor error")
elseif command == "SpawnedError" then
	task.spawn(error, "Controlled spawned error")
	return "Main finished"
elseif command == "Exit" then
	process.exit(actor.args[2])
elseif command == "Spin" then
	while true do
	end
elseif command == "ProtectedSpin" then
	while true do
		pcall(function()
			while true do
			end
		end)
	end
elseif command == "Idle" then
	task.wait(1000)
elseif command == "ReturnFunction" then
	return (function() end) :: any
end

return command
