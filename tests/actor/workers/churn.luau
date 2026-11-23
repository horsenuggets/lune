const roblox = require("@lune/roblox")

-- Creates and destroys instances, so it uses the shared instance lock continuously

for _ = 1, 2_000 do
	const folder = roblox.Instance.new("Folder")
	folder:Destroy()
end

return true
