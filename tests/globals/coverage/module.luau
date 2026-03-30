local Module = {}

function Module.greet(name)
	return "Hello, " .. name
end

function Module.farewell(name)
	return "Goodbye, " .. name
end

function Module.outer()
	local function inner()
		return 42
	end
	return inner()
end

function Module.unused()
	return "never called"
end

return Module
