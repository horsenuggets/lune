local Module = {}

function Module.greet(name)
	return "Hello, " .. name
end

function Module.farewell(name)
	return "Goodbye, " .. name
end

return Module
