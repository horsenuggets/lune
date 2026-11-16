-- Typedefs share one Instance type, so they can't describe the Value properties
local roblox = require("@lune/roblox") :: any
local Instance = roblox.Instance

-- Assigns a value and reads back what the property stored
local function assign(instance: any, value: any): any
	instance.Value = value
	return instance.Value
end

local function rejects(instance: any, value: any): boolean
	return not pcall(assign, instance, value)
end

-- Numbers assigned to string properties become their tostring form

local stringValue = Instance.new("StringValue")

assert(assign(stringValue, 5) == "5")
assert(assign(stringValue, 1.5) == "1.5")
assert(assign(stringValue, -0.25) == tostring(-0.25))

-- Numeric strings assigned to number properties become numbers

local numberValue = Instance.new("NumberValue")

assert(assign(numberValue, "5") == 5)
assert(assign(numberValue, "  2.5  ") == 2.5)
assert(assign(numberValue, "0x10") == 16)
assert(assign(Instance.new("IntValue"), "42") == 42)

-- Strings that aren't numbers still fail, without changing the value

assert(rejects(numberValue, "five"), "Non-numeric strings should not convert to numbers")
assert(rejects(numberValue, ""), "Empty strings should not convert to numbers")
assert(numberValue.Value == 16, "A failed assignment should not change the value")

-- Booleans and other values are never coerced

assert(rejects(stringValue, true), "Booleans should not convert to strings")
assert(rejects(stringValue, {}), "Tables should not convert to strings")
assert(rejects(numberValue, false), "Booleans should not convert to numbers")

local boolValue = Instance.new("BoolValue")
assert(rejects(boolValue, 1), "Numbers should not convert to booleans")
assert(rejects(boolValue, "true"), "Strings should not convert to booleans")

-- Regular assignments are unaffected

assert(assign(stringValue, "text") == "text")
assert(assign(numberValue, 7) == 7)
