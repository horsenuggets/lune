local luau = require("@lune/luau")

-- _G is explicitly mutable in Luau, so use an ordinary global for the import probe.
local setProbe, originalProbe = luau.load([[
    return function(value)
        __luneSafeenvProbe = value
    end, __luneSafeenvProbe
]])()
local probe = { Value = 1 }
setProbe(probe)

-- Safe environments cache imported values at load time. Custom environments must
-- resolve the same import dynamically, without depending on shared-runner timings.
local TEST_SCRIPT = [[
    return __luneSafeenvProbe.Value
]]

local TEST_BYTECODE = luau.compile(TEST_SCRIPT, {
	optimizationLevel = 2,
	coverageLevel = 0,
	debugLevel = 0,
})

-- Load the bytecode with different configurations
local safeCodegenFunction = luau.load(TEST_BYTECODE, {
	debugName = "safeCodegenFunction",
	codegenEnabled = true,
})
local unsafeCodegenFunction = luau.load(TEST_BYTECODE, {
	debugName = "unsafeCodegenFunction",
	environment = {},
	injectGlobals = true,
	codegenEnabled = true,
})
local safeFunction = luau.load(TEST_BYTECODE, {
	debugName = "safeFunction",
	codegenEnabled = false,
})
local unsafeFunction = luau.load(TEST_BYTECODE, {
	debugName = "unsafeFunction",
	environment = {},
	injectGlobals = true,
	codegenEnabled = false,
})

for _, func in { safeCodegenFunction, unsafeCodegenFunction, safeFunction, unsafeFunction } do
	assert(func() == 1, "Every configuration must see the original import value")
end

probe.Value = 2
local safeCodegenValue = safeCodegenFunction()
local unsafeCodegenValue = unsafeCodegenFunction()
local safeValue = safeFunction()
local unsafeValue = unsafeFunction()
setProbe(originalProbe)

assert(safeCodegenValue == 1, "Codegen with the default environment must preserve cached imports")
assert(safeValue == 1, "The default environment must preserve cached imports without codegen")
assert(
	unsafeCodegenValue == 2,
	"A custom environment must resolve imports dynamically with codegen requested"
)
assert(unsafeValue == 2, "A custom environment must resolve imports dynamically without codegen")

-- Native generation and execution flags are checked directly by the standard-library
-- Rust tests, including platforms that cannot generate native code.
