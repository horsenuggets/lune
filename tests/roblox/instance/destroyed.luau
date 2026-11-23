const roblox = require("@lune/roblox")
const Instance = roblox.Instance

-- Misusing a destroyed instance must raise an ordinary error that pcall can catch,
-- and must leave every other instance exactly as it was

local function expectError(expected: string, callback: () -> ...any)
	const ok, message = pcall(callback)
	assert(not ok, `Expected an error containing "{expected}", but the call succeeded`)
	assert(
		string.find(tostring(message), expected, 1, true),
		`Expected an error containing "{expected}", got "{tostring(message)}"`
	)
end

-- Parenting to a destroyed instance is refused before anything moves
const home = Instance.new("Folder")
home.Name = "Home"
const child = Instance.new("Part")
child.Parent = home
const destroyedParent = Instance.new("Folder")
destroyedParent:Destroy()
expectError("the new parent has been destroyed", function()
	child.Parent = destroyedParent
end)
assert(child.Parent == home, "A refused reparent must keep the old parent")
assert(
	#home:GetChildren() == 1 and home:GetChildren()[1] == child,
	"The old parent must keep the child"
)
assert(child:GetFullName() == "Home.Part", "The child must stay fully attached")

-- A destroyed DataModel refuses children like any other destroyed instance
const destroyedModel = Instance.new("DataModel")
destroyedModel:Destroy()
expectError("the new parent has been destroyed", function()
	child.Parent = destroyedModel
end)
assert(child.Parent == home, "A refused cross-document reparent must keep the old parent")

-- Clearing a destroyed instance is a no-op, as it already has no children
destroyedModel:ClearAllChildren()
destroyedParent:ClearAllChildren()

-- Serializing a destroyed instance is an ordinary error
expectError("has been destroyed", function()
	roblox.serializePlace(destroyedModel)
end)
expectError("has been destroyed", function()
	roblox.serializeModel({ destroyedParent })
end)

-- An instance can never become its own ancestor
const grandchild = Instance.new("Folder")
grandchild.Parent = child
expectError("its own descendant", function()
	home.Parent = home
end)
expectError("its own descendant", function()
	home.Parent = grandchild
end)
assert(home.Parent == nil, "A refused circular reparent must keep the old parent")
assert(grandchild.Parent == child and child.Parent == home, "The subtree must be unchanged")

-- Every Instance.new shares one scratch document, which other live instances keep. A
-- deserialized model gets a document of its own, so destroying its last top-level
-- instance drops that whole document. Its instances keep the members Roblox keeps
-- readable after Destroy, and every other member is an ordinary error.
const source = Instance.new("Folder")
const sourcePart = Instance.new("Part")
sourcePart.Parent = source
const loaded = roblox.deserializeModel(roblox.serializeModel({ source }))
assert(#loaded == 1, "The model must load one top-level instance")
const loadedFolder = loaded[1]
const loadedPart = loadedFolder:FindFirstChildOfClass("Part")
assert(loadedPart, "The loaded model must keep its child")
loadedFolder:Destroy()
for _, instance in { loadedFolder, loadedPart } do
	assert(instance.Parent == nil, "A destroyed instance must read a nil Parent")
	assert(type(instance.ClassName) == "string", "A destroyed instance must keep its ClassName")
	assert(instance:IsA(instance.ClassName), "A destroyed instance must keep IsA")
	assert(type(instance:GetDebugId()) == "string", "A destroyed instance must keep GetDebugId")
	instance:ClearAllChildren()
	instance:Destroy()
	const members: { [string]: () -> ...any } = {
		Clone = function()
			return instance:Clone()
		end,
		FindFirstAncestor = function()
			return instance:FindFirstAncestor("Missing")
		end,
		FindFirstChild = function()
			return instance:FindFirstChild("Missing")
		end,
		GetAttribute = function()
			return instance:GetAttribute("Missing")
		end,
		GetAttributes = function()
			return instance:GetAttributes()
		end,
		GetChildren = function()
			return instance:GetChildren()
		end,
		GetDescendants = function()
			return instance:GetDescendants()
		end,
		GetFullName = function()
			return instance:GetFullName()
		end,
		GetTags = function()
			return instance:GetTags()
		end,
		HasTag = function()
			return instance:HasTag("Missing")
		end,
		Name = function()
			return instance.Name
		end,
		SetAttribute = function()
			instance:SetAttribute("Key", 1)
		end,
		SetName = function()
			instance.Name = "Renamed"
		end,
		ToString = function()
			return tostring(instance)
		end,
	}
	for name, member in members do
		const ok, message = pcall(member)
		assert(not ok, `"{name}" on a destroyed {instance.ClassName} must error`)
		assert(
			string.find(tostring(message), "has been destroyed", 1, true),
			`"{name}" on a destroyed {instance.ClassName} raised "{tostring(message)}"`
		)
	end
end
assert(loadedFolder:IsA("Folder") and loadedPart:IsA("Part"), "Destroyed classes must not change")
assert(
	source.Parent == nil and sourcePart.Parent == source,
	"The serialized source must be untouched"
)

-- Everything keeps working afterward
const fresh = Instance.new("Folder")
fresh.Parent = home
assert(#home:GetChildren() == 2, "New instances must still work normally")
