local roblox = require("@lune/roblox") :: any
local Random = roblox.Random
local Vector3 = roblox.Vector3

-- Every expected value below was captured from Random in Roblox Studio

local function assertError(callback: () -> ...any, message: string)
	local success, err = pcall(callback)
	assert(not success, `Expected an error containing "{message}".`)
	assert(
		string.find(tostring(err), message, 1, true) ~= nil,
		`Expected an error containing "{message}" but got "{err}".`
	)
end

local function assertType(value: any, expected: string)
	assert(typeof(value) == expected, `Expected type "{expected}" but got "{typeof(value)}".`)
end

-- Constructors

assertType(Random.new(), "Random")
assertType(Random.new(nil), "Random")
assertType(Random.new(1, 2), "Random")
assert(tostring(Random.new(0)) == "Random")
assert(Random.new("12") == Random.new(12))
assert(Random.new(" 0x10 ") == Random.new(16))

assertError(function()
	Random.new("abc")
end, "invalid argument #1 to 'new' (number expected, got string)")
assertError(function()
	Random.new({})
end, "invalid argument #1 to 'new' (number expected, got table)")
assertError(function()
	Random.new(true)
end, "invalid argument #1 to 'new' (number expected, got boolean)")

-- Seeds truncate toward zero, and anything outside the safe integer range acts like zero

local function first(seed: number): number
	return Random.new(seed):NextNumber()
end

assert(first(0) == 0.55145569047490739)
assert(first(0.99) == first(0))
assert(first(-0.5) == first(0))
assert(first(-1) == 0.0037155688242635435)
assert(first(-1.5) == first(-1))
assert(first(2 ^ 32 + 5) == 0.38286544760735486)
assert(first(9007199254740991) == 0.027684032073451197)
assert(first(-9007199254740991) == 0.22788703655698744)
for _, seed in { 2 ^ 53, -2 ^ 53, 2 ^ 60, 0 / 0, math.huge, -math.huge } do
	assert(first(seed) == first(0), `Expected seed {seed} to behave like zero.`)
end

-- NextNumber

local random = Random.new(123)
assert(random:NextNumber() == 0.55763420027724708)
assert(random:NextNumber() == 0.56777438777716005)
assert(random:NextNumber() == 0.93235765263651404)

random = Random.new(123)
assert(random:NextNumber(5, 10) == 7.7881710013862353)
assert(random:NextNumber(10, 5) == 7.8388719388858004)
assert(random:NextNumber(-1, 1) == 0.86471530527302809)
assert(Random.new(1):NextNumber(1, 2, 3) == 1.9484344206192203)
assert(Random.new(1):NextNumber(1, "2") == 1.9484344206192203)

random = Random.new(7)
assert(random:NextNumber(5, 5) == 5)
assert(random:NextNumber() == 0.4321572161309693)

random = Random.new(7)
local nan = random:NextNumber(-math.huge, math.huge)
assert(nan ~= nan)
assert(random:NextNumber(0, math.huge) == math.huge)

assertError(function()
	Random.new(1):NextNumber(1)
end, "Random:NextNumber requires 0 or 2 arguments")
assertError(function()
	Random.new(1):NextNumber(nil)
end, "Random:NextNumber requires 0 or 2 arguments")
assertError(function()
	Random.new(1):NextNumber(1, nil)
end, "invalid argument #3 (number expected, got nil)")
assertError(function()
	Random.new(1):NextNumber(nil, nil)
end, "invalid argument #2 (number expected, got nil)")
assertError(function()
	Random.new(1):NextNumber(1, "x")
end, "invalid argument #3 (number expected, got string)")

-- NextInteger

random = Random.new(123)
assert(random:NextInteger(1, 10) == 4)
assert(random:NextInteger(-5, 5) == 1)
assert(random:NextInteger(10, 1) == 1)
assert(random:NextInteger(0, 0) == 0)

random = Random.new(123)
assert(random:NextInteger(1.9, 5.9) == 2)
assert(random:NextInteger(-1.5, 3.7) == 1)

random = Random.new(123)
assert(random:NextInteger(-2 ^ 53, 2 ^ 53) == 1038245451569626)
assert(random:NextInteger(0, 2 ^ 40) == 624274541314)
assert(random:NextInteger(-2 ^ 31, 2 ^ 31) == 1856961978)

local SEED_ONE_INTEGERS = {
	{ 0, 2 ^ 31 - 1, 2062141372 },
	{ 0, 2 ^ 31, 2062141373 },
	{ -2 ^ 31, 0, -85342275 },
	{ 0, 2 ^ 32 - 2, 4073494817 },
	{ 0, 2 ^ 32 - 1, 4073494818 },
	{ 0, 2 ^ 32 + 1, 4073494819 },
	{ 0, 2 ^ 53 + 2, 8542737806572147 },
	{ -2 ^ 63, 2 ^ 63, -9.5121704584979674e+17 },
	{ 0, 2 ^ 64, 8.7477635139298775e+18 },
	{ -math.huge, 1, -4.7560852292489837e+17 },
	{ 0 / 0, 1, 1 },
	{ 0 / 0, 0 / 0, 0 },
}
for _, case in SEED_ONE_INTEGERS do
	local actual = Random.new(1):NextInteger(case[1], case[2])
	assert(
		actual == case[3],
		`Expected NextInteger({case[1]}, {case[2]}) to be {case[3]} but got {actual}.`
	)
end

assertError(function()
	Random.new(1):NextInteger()
end, "missing argument #2 (number expected)")
assertError(function()
	Random.new(1):NextInteger(1)
end, "missing argument #3 (number expected)")
assertError(function()
	Random.new(1):NextInteger(nil)
end, "invalid argument #2 (number expected, got nil)")
assertError(function()
	Random.new(1):NextInteger(1, {})
end, "invalid argument #3 (number expected, got table)")
assert(Random.new(1):NextInteger("1", "5") == Random.new(1):NextInteger(1, 5))

-- NextUnitVector

random = Random.new(123)
assert(
	random:NextUnitVector()
		== Vector3.new(-0.4335869252681732, 0.8937089443206787, 0.11526834964752197)
)
assert(
	random:NextUnitVector()
		== Vector3.new(0.9890706539154053, 0.05801483616232872, 0.13554883003234863)
)
assertType(Random.new(1):NextUnitVector(5), "Vector3")

-- Shuffle

random = Random.new(123)
local values = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 }
random:Shuffle(values)
assert(table.concat(values, ",") == "7,3,9,8,5,2,10,1,6,4")
assert(random:NextNumber() == 0.80665157926436382)

random = Random.new(5)
values = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, a = 1 }
random:Shuffle(values)
assert(table.concat(values, ",") == "4,6,9,3,5,10,2,1,8,7")
assert(values.a == 1)

-- Tables with zero or one element do not consume any values
random = Random.new(1)
random:Shuffle({ 1 })
random:Shuffle({})
assert(random == Random.new(1))

-- Metamethods are never invoked
local guarded = setmetatable({ 1, 2, 3 }, {
	__index = function()
		error("Shuffle should not invoke __index.")
	end,
	__newindex = function()
		error("Shuffle should not invoke __newindex.")
	end,
})
Random.new(1):Shuffle(guarded)

values = { 1, 2, 3 }
values[3] = nil
Random.new(1):Shuffle(values)
assert(#values == 2)

local HOLE_ERROR = "Attempted to shuffle table containing nil values"
assertError(function()
	Random.new(1):Shuffle({ 1, nil, 3 } :: { any })
end, HOLE_ERROR)
assertError(function()
	Random.new(1):Shuffle({ nil, 2 } :: { any })
end, HOLE_ERROR)
assertError(function()
	Random.new(1):Shuffle(table.freeze({ 1, nil, 3 } :: { any }))
end, HOLE_ERROR)
assertError(function()
	Random.new(1):Shuffle(table.freeze({ 1, 2, 3 }))
end, "attempt to modify a readonly table")
Random.new(1):Shuffle(table.freeze({ 1 }))
assertError(function()
	Random.new(1):Shuffle()
end, "missing argument #2 (table expected)")
assertError(function()
	Random.new(1):Shuffle(nil)
end, "invalid argument #2 (table expected, got nil)")
assertError(function()
	Random.new(1):Shuffle("x")
end, "invalid argument #2 (table expected, got string)")

-- Clone & equality

random = Random.new(42)
random:NextNumber()
local clone = random:Clone()
assert(clone == random)
assert(not rawequal(clone, random))
assert(clone:NextNumber() == random:NextNumber())
random:NextNumber()
assert(clone ~= random)
assert(Random.new(1) == Random.new(1))
assert(Random.new(1) ~= Random.new(2))
assert(Random.new(1) ~= (1 :: any))

-- Methods called with an invalid self

assertError(function()
	Random.new(1).NextNumber()
end, "missing argument #1 to 'NextNumber' (Random expected)")
assertError(function()
	Random.new(1).NextInteger(nil, 1, 2)
end, "invalid argument #1 to 'NextInteger' (Random expected, got nil)")
assertError(function()
	Random.new(1).NextUnitVector({})
end, "invalid argument #1 to 'NextUnitVector' (Random expected, got table)")
assertError(function()
	Random.new(1).Shuffle(1, {})
end, "invalid argument #1 to 'Shuffle' (Random expected, got number)")
assertError(function()
	Random.new(1).Clone(Vector3.new())
end, "invalid argument #1 to 'Clone' (Random expected, got Vector3)")

-- Members

assertError(function()
	return Random.new(1).Foo
end, "Foo is not a valid member of Random")
assertError(function()
	return Random.new(1).nextNumber
end, "nextNumber is not a valid member of Random")
assertError(function()
	Random.new(1).Foo = 1
end, "Foo is not a valid member of Random")
assertError(function()
	Random.new(1).NextNumber = 1
end, "NextNumber is not a valid member of Random")
assert(Random.NextNumber == nil)
assert(not pcall(function()
	Random.extra = true
end))
