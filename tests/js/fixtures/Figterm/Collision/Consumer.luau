--[[

Consumer

Imports a same-named module from a nested directory to exercise provider identity.

--]]

return function(Context: any)
	return Context:Import("./Value.ts")
end
