local util = require("./util")
local pass = util.pass

local baseUrl = ""
local visited = {}
local paths = { "/redirect/3", "/redirect/2", "/redirect/1", "/redirect/0" }

util.withServer(function(request)
	table.insert(visited, request.path)
	local remaining = tonumber(request.path:match("^/redirect/(%d+)$"))
	assert(remaining ~= nil, "Unexpected redirect path.")
	if remaining > 0 then
		return {
			status = 302,
			headers = { Location = `{baseUrl}/redirect/{remaining - 1}` },
			body = "",
		}
	end
	return "Redirects complete."
end, function(url)
	baseUrl = url
	pass("GET", `{url}/redirect/3`, "Redirect 3 times")
	assert(#visited == #paths, "Expected exactly three redirects and one final request.")
	for index, path in paths do
		assert(visited[index] == path, "Unexpected redirect request order.")
	end
end)
