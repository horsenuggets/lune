-- CPU-bound work shared by the serial and parallel halves of the actor benchmark

return function(iterations: number): number
	local total = 0
	for index = 1, iterations do
		total += math.sqrt(index) % 7
	end
	return total
end
