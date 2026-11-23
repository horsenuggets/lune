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

-- A destroyed DataModel is the root of its own document, which is gone entirely
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

-- Everything keeps working afterward
const fresh = Instance.new("Folder")
fresh.Parent = home
assert(#home:GetChildren() == 2, "New instances must still work normally")
