-- Typedefs share one Instance type, so they can't describe CollectionService's signatures
local roblox = require("@lune/roblox") :: any
local Instance = roblox.Instance

local game = Instance.new("DataModel")
local CollectionService = game:GetService("CollectionService")

local TAG_NAME = "CollectionTagName"

local part = Instance.new("Part")
local other = Instance.new("Part")

-- CollectionService tags the instance passed as its first argument

assert(CollectionService:HasTag(part, TAG_NAME) == false)

CollectionService:AddTag(part, TAG_NAME)

assert(CollectionService:HasTag(part, TAG_NAME) == true)
assert(part:HasTag(TAG_NAME) == true)
assert(other:HasTag(TAG_NAME) == false)
assert(
	CollectionService:HasTag(TAG_NAME) == false,
	"Tagging an instance should not tag the service"
)

local tags = CollectionService:GetTags(part)
assert(#tags == 1)
assert(tags[1] == TAG_NAME)
assert(#CollectionService:GetTags(other) == 0)

CollectionService:RemoveTag(part, TAG_NAME)

assert(CollectionService:HasTag(part, TAG_NAME) == false)
assert(part:HasTag(TAG_NAME) == false)

-- Tags added through the instance are visible through the service, and the reverse

other:AddTag(TAG_NAME)
assert(CollectionService:HasTag(other, TAG_NAME) == true)
assert(CollectionService:GetTags(other)[1] == TAG_NAME)

-- The service can still tag itself when given only a tag

CollectionService:AddTag(TAG_NAME)
assert(CollectionService:HasTag(TAG_NAME) == true)
assert(CollectionService:GetTags()[1] == TAG_NAME)
assert(part:HasTag(TAG_NAME) == false)

CollectionService:RemoveTag(TAG_NAME)
assert(CollectionService:HasTag(TAG_NAME) == false)

-- Other instances never treat an instance argument as their target

local model = Instance.new("Model")
local success = pcall(function()
	model:AddTag(part, TAG_NAME)
end)
assert(not success, "Only CollectionService should accept an instance argument")
assert(part:HasTag(TAG_NAME) == false)
assert(#model:GetTags() == 0)

-- Destroyed targets are rejected

local destroyed = Instance.new("Part")
destroyed:Destroy()
assert(not pcall(function()
	CollectionService:AddTag(destroyed, TAG_NAME)
end))

-- Missing tags are still argument errors

assert(not pcall(function()
	CollectionService:AddTag(part)
end))
