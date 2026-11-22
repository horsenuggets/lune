local net = require("@lune/net")
local serde = require("@lune/serde")
local util = require("./util")

local runtime, version = table.unpack(_VERSION:split(" "))
local expectedUserAgent = runtime:lower() .. "/" .. version

util.withServer(function(request)
	return {
		headers = { ["Content-Type"] = "application/json" },
		body = serde.encode("json", { { ua = { rawUa = request.headers["user-agent"] } } }),
	}
end, function(url)
	local userAgent: string = serde.decode("json", net.request(url).body)[1].ua.rawUa
	assert(userAgent == expectedUserAgent, "Expected user agent to be " .. expectedUserAgent)
end)
