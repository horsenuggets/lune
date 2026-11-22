local net = require("@lune/net")
local stdio = require("@lune/stdio")

local util = {}

function util.pass(method, url, message)
	local success, response = pcall(net.request, {
		method = method,
		url = url,
	})
	if not success then
		error(`{message} errored!\nError message: {tostring(response)}`)
	elseif not response.ok then
		error(
			`{message} failed, but should have passed!`
				.. `\nStatus code: {response.statusCode}`
				.. `\nStatus message: {response.statusMessage}`
				.. `\nResponse headers: {stdio.format(response.headers)}`
				.. `\nResponse body: {response.body}`
		)
	end
end

function util.fail(method, url, message)
	local success, response = pcall(net.request, {
		method = method,
		url = url,
	})
	if not success then
		error(`{message} errored!\nError message: {tostring(response)}`)
	elseif response.ok then
		error(
			`{message} passed, but should have failed!`
				.. `\nStatus code: {response.statusCode}`
				.. `\nStatus message: {response.statusMessage}`
				.. `\nResponse headers: {stdio.format(response.headers)}`
				.. `\nResponse body: {response.body}`
		)
	end
end

function util.withServer(handleRequest, callback)
	assert(type(handleRequest) == "function")
	assert(type(callback) == "function")
	local handle = net.serve(0, {
		address = "http://127.0.0.1",
		handleRequest = handleRequest,
	})
	local success, err = pcall(function()
		assert(handle.port > 0, "Server handle did not report its assigned port.")
		callback(`http://127.0.0.1:{handle.port}`)
	end)
	handle:stop()
	if not success then
		error(err, 0)
	end
end

return util
