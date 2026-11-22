local net = require("@lune/net")

local METHODS = { "GET", "POST", "PATCH", "PUT", "DELETE" }

local handle = net.serve(0, {
	address = "http://127.0.0.1",
	handleRequest = function(request)
		return request.method
	end,
})

local success, err = pcall(function()
	assert(handle.port > 0, "Server handle did not report its assigned port")
	for _, method in METHODS do
		local response = net.request({
			url = `http://127.0.0.1:{handle.port}`,
			method = method :: net.HttpMethod,
		})
		assert(response.ok, `Request method "{method}" failed with status {response.statusCode}`)
		assert(response.body == method, `Request method "{method}" was not echoed correctly`)
	end
end)

handle:stop()
if not success then
	error(err, 0)
end
