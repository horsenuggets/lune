--[[

globals

Verifies that Roblox module exports are available as identical globals and work without
Studio discovery, credentials, or filesystem access.

--]]

-- Read the actual environment before requiring the module so lazy require cannot hide
-- missing setup. This runtime fixture also runs under the standard-platform analyzer.
const environment = getfenv()
const part = environment.Instance.new("Part")
part.Name = "GlobalPart"
part.Anchored = true
part.Color = environment.Color3.fromRGB(32, 64, 128)
part.Material = environment.Enum.Material.Neon
part.Size = environment.Vector3.new(2, 4, 6)
part.CFrame = environment.CFrame.new(1, 2, 3)
part:SetAttribute("Offset", environment.Vector2.new(4, 5))
part:SetAttribute("Padding", environment.UDim.new(0.5, 8))
part:SetAttribute("Bounds", environment.UDim2.new(0.1, 2, 0.3, 4))
part:SetAttribute("Range", environment.NumberRange.new(2, 5))

for _, asXml in { false, true } do
	const encoded = environment.serializeModel({ part }, asXml)
	const decoded = environment.deserializeModel(encoded)
	assert(#decoded == 1)
	const restored = decoded[1]
	assert(restored.ClassName == "Part")
	assert(restored.Name == part.Name)
	assert(restored.Anchored == part.Anchored)
	assert(restored.Color == part.Color)
	assert(restored.Material == part.Material)
	assert(restored.Size == part.Size)
	assert(restored.CFrame == part.CFrame)
	for name, value in part:GetAttributes() do
		assert(
			restored:GetAttribute(name) == value,
			`Expected attribute "{name}" to survive serialization.`
		)
	end
	restored:Destroy()
end

const roblox = require("@lune/roblox")
for name, value in roblox do
	assert(type(name) == "string")
	assert(
		rawequal(environment[name], value),
		`Expected global "{name}" to match its module export.`
	)
end
assert(
	rawequal(require("@lune/roblox"), roblox),
	"Expected repeated require to preserve module identity."
)

assert(type(environment.Vector3.new(1, 2, 3)) == "userdata")
for expectedType, value in
	{
		CFrame = environment.CFrame.new(),
		Color3 = environment.Color3.new(),
		Enum = environment.Enum.Material,
		EnumItem = environment.Enum.Material.Neon,
		Enums = environment.Enum,
		Instance = part,
		Vector3 = environment.Vector3.new(1, 2, 3),
	}
do
	assert(typeof(value) == expectedType)
end
part:Destroy()
