-- Runs a function inside this actor and returns its raw coverage data to the owner

local function measured(value: number): number
	if value > 0 then
		return value * 2
	end
	return 0
end

measured(1)
measured(2)

return debug.getcoverage(measured)
