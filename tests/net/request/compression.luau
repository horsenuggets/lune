local net = require("@lune/net")
local serde = require("@lune/serde")

local JSON = '{"message":"Hello, lune!","gzipped":true}'
local COMPRESSED = serde.compress("gzip", JSON)

local handle = net.serve(0, {
	address = "http://127.0.0.1",
	handleRequest = function()
		return {
			body = COMPRESSED,
			headers = {
				["Content-Encoding"] = "gzip",
				["Content-Type"] = "application/json",
			},
		}
	end,
})

local success, err = pcall(function()
	-- Should decompress automatically by default

	local response = net.request({
		url = `http://127.0.0.1:{handle.port}`,
		headers = {
			["Accept-Encoding"] = "gzip",
		} :: { [string]: string },
	})

	assert(
		response.ok,
		"Request failed with status "
			.. tostring(response.statusCode)
			.. " "
			.. tostring(response.statusMessage)
	)

	local decoded, json = pcall(serde.decode, "json" :: "json", response.body)
	assert(decoded, "Failed to decode json response\n" .. tostring(json))
	assert(response.body == JSON, "Automatic decompression did not return the exact payload")

	-- Content encoding header should no longer exist when automatically decompressed

	assert(
		response.headers["content-encoding"] == nil,
		"Content encoding header still exists after automatic decompression"
	)

	-- Should do nothing when explicitly disabled

	local response2 = net.request({
		url = `http://127.0.0.1:{handle.port}`,
		headers = {
			["Accept-Encoding"] = "gzip",
		} :: { [string]: string },
		options = { decompress = false },
	})

	assert(
		response2.ok,
		"Request failed with status "
			.. tostring(response2.statusCode)
			.. " "
			.. tostring(response2.statusMessage)
	)

	local decoded2 = pcall(serde.decode, "json" :: "json", response2.body)
	assert(not decoded2, "Decompression disabled still returned json response")
	assert(response2.body == COMPRESSED, "Decompression disabled did not preserve raw bytes")
	assert(
		serde.decompress("gzip", response2.body) == JSON,
		"Raw response did not decompress to the exact payload"
	)

	-- Content encoding header should still exist when not automatically decompressed

	assert(
		response2.headers["content-encoding"] == "gzip",
		"Content encoding header is missing or incorrect when automatic decompression is disabled"
	)
end)

handle:stop()
if not success then
	error(err, 0)
end
