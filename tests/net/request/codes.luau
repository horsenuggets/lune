local util = require("./util")
local pass, fail = util.pass, util.fail

util.withServer(function(request)
	local status = tonumber(request.path:match("^/status/(%d+)$"))
	assert(status ~= nil, "Unexpected status path.")
	return { status = status, body = "" }
end, function(url)
	pass("GET", `{url}/status/200`, "Request status code - 200")
	fail("GET", `{url}/status/400`, "Request status code - 400")
	fail("GET", `{url}/status/500`, "Request status code - 500")
end)
