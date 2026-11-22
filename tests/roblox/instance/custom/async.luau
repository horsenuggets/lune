local net = require("@lune/net")
local roblox = require("@lune/roblox")
local serde = require("@lune/serde")
local util = require("../../../net/request/util")

roblox.implementMethod("HttpService", "GetAsync", function(_, url: string)
	local response = net.request({
		method = "GET",
		url = url,
	})
	return response.body
end)

roblox.implementMethod("HttpService", "JSONDecode", function(_, value)
	return serde.decode("json", value)
end)

-- Reference: https://create.roblox.com/docs/reference/engine/classes/HttpService#GetAsync

local game = roblox.Instance.new("DataModel")
local HttpService = game:GetService("HttpService") :: any

util.withServer(function(request)
	assert(request.method == "GET", "Expected the async method to issue a GET request.")
	assert(request.path == "/json", "Expected the async method to preserve the request path.")
	return {
		headers = { ["Content-Type"] = "application/json" },
		body = serde.encode("json", { slideshow = { title = "Local slideshow" } }),
	}
end, function(url)
	local response = HttpService:GetAsync(`{url}/json`)
	local data = HttpService:JSONDecode(response)

	assert(type(data) == "table", "Returned JSON data should decode to a table")
	assert(type(data.slideshow) == "table", "Returned JSON data should contain 'slideshow'")
	assert(data.slideshow.title == "Local slideshow", "Expected the local server's JSON body.")
end)
game:Destroy()
