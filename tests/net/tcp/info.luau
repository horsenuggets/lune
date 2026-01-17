local net = require("@lune/net")

local stream = net.tcp.connect("httpbingo.org", 80)

assert(type(stream.localIp) == "string", "localIp should be a string")
assert(type(stream.localPort) == "number", "localPort should be a number")
assert(type(stream.remoteIp) == "string", "remoteIp should be a string")
assert(stream.remotePort == 80, "remotePort should be 80")

-- Check for either IPv4 (e.g., 192.168.1.1) or IPv6 (e.g., 2001:db8::1)
local isIpv4 = string.match(stream.remoteIp, "^%d+%.%d+%.%d+%.%d+$") ~= nil
local isIpv6 = string.match(stream.remoteIp, "^[%x:]+$") ~= nil
assert(isIpv4 or isIpv6, "remoteIp should be a valid IP address")

assert(stream.localPort > 0 and stream.localPort <= 65535, "localPort should be in valid range")

stream:close()
