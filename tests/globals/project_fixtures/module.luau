-- Returns the project path seen at module load time, plus a function that reads it later
-- from a different calling script

return {
	LoadTimePath = tostring(project),
	getPath = function()
		return tostring(project)
	end,
}
