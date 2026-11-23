const roblox = require("@lune/roblox")

-- Uses a class member of a destroyed DataModel, which used to panic while holding the
-- process-wide instance lock, then keeps using instances to prove nothing broke.

const dataModel = roblox.Instance.new("DataModel")
dataModel:Destroy()

const ok, message = pcall(function()
	return dataModel:GetService("Workspace")
end)

const folder = roblox.Instance.new("Folder")
folder.Name = "AfterDestroyedService"
const name = folder.Name
folder:Destroy()

return {
	Message = tostring(message),
	Name = name,
	Ok = ok,
}
