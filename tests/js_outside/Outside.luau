--[[

Outside

Imports an absolute module from a caller outside the JavaScript module root.

--]]

return function(Context: any)
	return Context:Import("/fixtures/Figterm/Catalog.ts")
end
