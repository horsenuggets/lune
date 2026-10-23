local fs = require("@lune/fs")
local task = require("@lune/task")

type WatchEvent = fs.WatchEvent

local ROOT = "bin/fs_watch"
local SETTLE = 0.25
local TIMEOUT = 10

-- Make sure we start from a clean slate, other fs tests
-- also write to the bin dir, so we only ever watch our own

if fs.isDir(ROOT) then
	fs.removeDir(ROOT)
end
fs.writeDir(ROOT)

local function makeDir(name: string): string
	local dir = `{ROOT}/{name}`
	fs.writeDir(dir)
	return dir
end

local function normalize(path: string): string
	return (string.gsub(path, "\\", "/"))
end

local function involves(event: WatchEvent, suffix: string): boolean
	for _, path in event.paths do
		local normalized = normalize(path)
		if string.sub(normalized, -#suffix) == suffix then
			return true
		end
	end
	return false
end

local function find(events: { WatchEvent }, suffix: string, kind: string?): WatchEvent?
	for _, event in events do
		if involves(event, suffix) and (kind == nil or event.kind == kind) then
			return event
		end
	end
	return nil
end

local function waitFor(predicate: () -> boolean, message: string)
	local elapsed = 0
	while not predicate() do
		if elapsed >= TIMEOUT then
			error(`Timed out after {TIMEOUT} seconds waiting for {message}`)
		end
		elapsed += task.wait(0.05)
	end
end

-- Invalid arguments should throw errors

assert(
	not pcall(fs.watch, `{ROOT}/does_not_exist`, function() end),
	"Watching a missing path should throw"
)
assert(not pcall(fs.watch :: any, ROOT), "Watching without a callback should throw")
assert(
	not pcall(fs.watch :: any, ROOT, { debounce = -1 }, function() end),
	"Watching with a negative debounce should throw"
)
assert(
	not pcall(fs.watch :: any, ROOT, true, function() end),
	"Watching with non-table options should throw"
)

-- Watching a directory should report creating, modifying and removing files

do
	local dir = makeDir("dir")
	local events: { WatchEvent } = {}
	local watcher = fs.watch(dir, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/a.txt`, "one")
	waitFor(function()
		return find(events, "/dir/a.txt", "create") ~= nil
	end, "a create event in a watched directory")

	fs.writeFile(`{dir}/a.txt`, "two")
	waitFor(function()
		return find(events, "/dir/a.txt", "modify") ~= nil
	end, "a modify event in a watched directory")

	fs.removeFile(`{dir}/a.txt`)
	waitFor(function()
		return find(events, "/dir/a.txt", "remove") ~= nil
	end, "a remove event in a watched directory")

	for _, event in events do
		for _, path in event.paths do
			local absolute = string.sub(path, 1, 1) == "/" or string.match(path, "^%a:[\\/]") ~= nil
			assert(absolute, `Event path "{path}" should be absolute`)
		end
	end

	watcher.stop()
	assert(not pcall(watcher.stop), "Stopping a watcher twice should throw")
end

-- Directories should be watched recursively by default, and only then

do
	local dir = makeDir("recursive")
	fs.writeDir(`{dir}/sub`)
	local events: { WatchEvent } = {}
	local watcher = fs.watch(dir, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/sub/b.txt`, "nested")
	waitFor(function()
		return find(events, "/recursive/sub/b.txt") ~= nil
	end, "an event for a nested file with a recursive watcher")
	watcher.stop()
end

do
	local dir = makeDir("non_recursive")
	fs.writeDir(`{dir}/sub`)
	local events: { WatchEvent } = {}
	local watcher = fs.watch(dir, { recursive = false }, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/sub/c.txt`, "nested")
	task.wait(SETTLE)
	fs.writeFile(`{dir}/marker.txt`, "top level")
	waitFor(function()
		return find(events, "/non_recursive/marker.txt") ~= nil
	end, "an event for a top level file with a non-recursive watcher")

	assert(
		find(events, "/non_recursive/sub/c.txt") == nil,
		"A non-recursive watcher should not report nested files"
	)
	watcher.stop()
end

-- Watching a single file should ignore its siblings, and
-- keep working after the file is replaced by an atomic save

do
	local dir = makeDir("single")
	local target = `{dir}/config.json`
	fs.writeFile(target, "{}")
	local events: { WatchEvent } = {}
	local watcher = fs.watch(target, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/sibling.txt`, "ignored")
	fs.writeFile(`{dir}/.config.json.tmp`, `\{"saved": 1}`)
	fs.move(`{dir}/.config.json.tmp`, target, true)
	waitFor(function()
		return find(events, "/single/config.json") ~= nil
	end, "an event after atomically replacing a watched file")

	local countBefore = #events
	fs.writeFile(target, `\{"saved": 2}`)
	waitFor(function()
		for index = countBefore + 1, #events do
			if involves(events[index], "/single/config.json") then
				return true
			end
		end
		return false
	end, "an event for a watched file after it was replaced")

	for _, event in events do
		local paths = table.concat(event.paths, ", ")
		assert(
			involves(event, "/single/config.json"),
			`Every event for a watched file should involve it, got "{event.kind}" for "{paths}"`
		)
	end
	watcher.stop()
end

-- Debouncing should merge identical events into one callback

do
	local dir = makeDir("debounce")
	local events: { WatchEvent } = {}
	local watcher = fs.watch(dir, { debounce = 0.3 }, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)

	for index = 1, 5 do
		fs.writeFile(`{dir}/burst.txt`, tostring(index))
	end
	waitFor(function()
		return find(events, "/debounce/burst.txt") ~= nil
	end, "a debounced event")
	task.wait(0.6)

	local seen = {}
	for _, event in events do
		local key = `{event.kind} {table.concat(event.paths, ", ")}`
		assert(not seen[key], `Debounced events should not contain duplicates, got "{key}" twice`)
		seen[key] = true
	end
	watcher.stop()
end

-- Callbacks should be able to yield, and should never overlap

do
	local dir = makeDir("sequential")
	local active, maxActive, completed = 0, 0, 0
	local watcher = fs.watch(dir, function()
		active += 1
		maxActive = math.max(maxActive, active)
		task.wait(0.05)
		active -= 1
		completed += 1
	end)
	task.wait(SETTLE)

	for index = 1, 3 do
		fs.writeFile(`{dir}/file{index}.txt`, "contents")
	end
	waitFor(function()
		return completed >= 3
	end, "three yielding callbacks to complete")

	assert(maxActive == 1, `Callbacks should never overlap, but {maxActive} ran at once`)
	watcher.stop()
end

-- Stopping from inside a callback should prevent any further callbacks

do
	local dir = makeDir("stop_inside")
	local calls = 0
	local watcher
	watcher = fs.watch(dir, function()
		calls += 1
		watcher.stop()
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/first.txt`, "contents")
	waitFor(function()
		return calls >= 1
	end, "the first callback of a self-stopping watcher")

	for index = 1, 3 do
		fs.writeFile(`{dir}/after{index}.txt`, "contents")
	end
	task.wait(SETTLE * 2)

	assert(calls == 1, `No callbacks should run after stopping, but {calls} ran`)
end

-- Clean up, all watchers are stopped so the process is free to exit

fs.removeDir(ROOT)
