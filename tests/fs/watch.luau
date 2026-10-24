const fs = require("@lune/fs")
const task = require("@lune/task")

type WatchCallback = fs.WatchCallback
type WatchEvent = fs.WatchEvent
type WatchHandle = fs.WatchHandle
type WatchOptions = fs.WatchOptions

const ROOT = "bin/fs_watch"
const SETTLE = 0.25
const TIMEOUT = 10

-- Make sure we start from a clean slate, other fs tests
-- also write to the bin dir, so we only ever watch our own

if fs.isDir(ROOT) then
	fs.removeDir(ROOT)
end
fs.writeDir(ROOT)

-- Every watcher is tracked so that it can be stopped even when a test fails,
-- otherwise the active watcher would keep the process alive and hang the test

const activeWatchers: { WatchHandle } = {}

const function watch(path: string, options: WatchOptions?, callback: WatchCallback): WatchHandle
	const watcher = fs.watch(path, options, callback)
	table.insert(activeWatchers, watcher)
	return watcher
end

const function test(name: string, body: () -> ())
	const success, err = pcall(body :: () -> any)
	for _, watcher in activeWatchers do
		pcall(watcher.stop)
	end
	table.clear(activeWatchers)
	if not success then
		error(`Test "{name}" failed - {err}`)
	end
end

const function makeDir(name: string): string
	const dir = `{ROOT}/{name}`
	fs.writeDir(dir)
	return dir
end

const function normalize(path: string): string
	return (string.gsub(path, "\\", "/"))
end

const function involves(event: WatchEvent, suffix: string): boolean
	for _, path in event.paths do
		const normalized = normalize(path)
		if string.sub(normalized, -#suffix) == suffix then
			return true
		end
	end
	return false
end

const function find(events: { WatchEvent }, suffix: string, kind: string?): WatchEvent?
	for _, event in events do
		if involves(event, suffix) and (kind == nil or event.kind == kind) then
			return event
		end
	end
	return nil
end

const function waitFor(predicate: () -> boolean, message: string)
	local elapsed = 0
	while not predicate() do
		if elapsed >= TIMEOUT then
			error(`Timed out after {TIMEOUT} seconds waiting for {message}`)
		end
		elapsed += task.wait(0.05)
	end
end

test("invalid arguments", function()
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
		not pcall(fs.watch :: any, ROOT, { debounce = 1e20 }, function() end),
		"Watching with an unrepresentable debounce should throw"
	)
	assert(
		not pcall(fs.watch :: any, ROOT, true, function() end),
		"Watching with non-table options should throw"
	)
end)

test("directory watching", function()
	const dir = makeDir("dir")
	const events: { WatchEvent } = {}
	const watcher = watch(dir, nil, function(event)
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
			const absolute = string.sub(path, 1, 1) == "/" or string.match(path, "^%a:[\\/]") ~= nil
			assert(absolute, `Event path "{path}" should be absolute`)
		end
	end

	watcher.stop()
	assert(not pcall(watcher.stop), "Stopping a watcher twice should throw")
end)

test("callback without options", function()
	const dir = makeDir("no_options")
	const events: { WatchEvent } = {}
	const watcher = fs.watch(dir, function(event)
		table.insert(events, event)
	end)
	table.insert(activeWatchers, watcher)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/a.txt`, "one")
	waitFor(function()
		return find(events, "/no_options/a.txt") ~= nil
	end, "an event for a watcher created without options")
end)

test("recursive watching", function()
	const dir = makeDir("recursive")
	fs.writeDir(`{dir}/sub`)
	const events: { WatchEvent } = {}
	watch(dir, nil, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/sub/b.txt`, "nested")
	waitFor(function()
		return find(events, "/recursive/sub/b.txt") ~= nil
	end, "an event for a nested file with a recursive watcher")
end)

test("non-recursive watching", function()
	const dir = makeDir("non_recursive")
	fs.writeDir(`{dir}/sub`)
	const events: { WatchEvent } = {}
	watch(dir, { recursive = false }, function(event)
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
end)

test("single file watching with atomic saves", function()
	const dir = makeDir("single")
	const target = `{dir}/config.json`
	fs.writeFile(target, "{}")
	const events: { WatchEvent } = {}
	watch(target, nil, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/sibling.txt`, "ignored")
	fs.writeFile(`{dir}/.config.json.tmp`, `\{"saved": 1}`)
	fs.move(`{dir}/.config.json.tmp`, target, true)
	waitFor(function()
		return find(events, "/single/config.json") ~= nil
	end, "an event after atomically replacing a watched file")

	const countBefore = #events
	fs.writeFile(target, `\{"saved": 2}`)
	waitFor(function()
		for index = countBefore + 1, #events do
			if involves(events[index], "/single/config.json") then
				return true
			end
		end
		return false
	end, "an event for a watched file after it was replaced")

	-- Events without paths are rescan notifications, and are always delivered
	for _, event in events do
		const paths = table.concat(event.paths, ", ")
		assert(
			#event.paths == 0 or involves(event, "/single/config.json"),
			`Every event for a watched file should involve it, got "{event.kind}" for "{paths}"`
		)
	end
end)

-- Watchers only ever report a file's stored name, so watching a file by another
-- spelling (possible on case-insensitive filesystems) must fail loudly instead of
-- silently receiving nothing
test("single file watching requires the stored spelling", function()
	const dir = makeDir("casing")
	fs.writeFile(`{dir}/config.json`, "{}")
	if not fs.isFile(`{dir}/CONFIG.json`) then
		return -- Case-sensitive filesystem, nothing to test here
	end

	-- Goes through the helper so an unexpectedly successful watch still gets stopped
	const success, err = pcall(watch, `{dir}/CONFIG.json`, nil, function() end)
	assert(not success, "Watching a file by a different spelling should throw")
	assert(
		string.find(tostring(err), "config.json", 1, true) ~= nil,
		`The error should name the stored spelling, got "{err}"`
	)

	const events: { WatchEvent } = {}
	watch(`{dir}/config.json`, nil, function(event)
		table.insert(events, event)
	end)
	task.wait(SETTLE)
	fs.writeFile(`{dir}/config.json`, `\{"saved": 1}`)
	waitFor(function()
		return find(events, "/casing/config.json") ~= nil
	end, "an event for a file watched by its stored spelling")
end)

test("debouncing", function()
	const dir = makeDir("debounce")
	const events: { WatchEvent } = {}
	watch(dir, { debounce = 0.3 }, function(event)
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

	const seen = {}
	for _, event in events do
		const key = `{event.kind} {table.concat(event.paths, ", ")}`
		assert(not seen[key], `Debounced events should not contain duplicates, got "{key}" twice`)
		seen[key] = true
	end
end)

test("sequential yielding callbacks", function()
	const dir = makeDir("sequential")
	local active, maxActive, completed = 0, 0, 0
	watch(dir, nil, function()
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
end)

test("stopping inside a callback", function()
	const dir = makeDir("stop_inside")
	local calls = 0
	local watcher
	watcher = watch(dir, nil, function()
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
end)

-- A callback that suspends forever must not keep a stopped watcher alive,
-- if it did then this test would hang instead of letting the process exit

test("stopping while a callback is suspended", function()
	const dir = makeDir("suspended")
	local suspended = false
	const watcher = watch(dir, nil, function()
		suspended = true
		coroutine.yield()
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/first.txt`, "contents")
	waitFor(function()
		return suspended
	end, "a callback to suspend")

	watcher.stop()
end)

-- A callback that is still waiting when its watcher stops must be able to
-- finish afterwards, even though the watcher no longer waits for its result

test("stopping while a callback is waiting", function()
	const dir = makeDir("waiting")
	local waiting, finished = false, false
	const watcher = watch(dir, nil, function()
		if waiting then
			return
		end
		waiting = true
		task.wait(0.3)
		finished = true
	end)
	task.wait(SETTLE)

	fs.writeFile(`{dir}/first.txt`, "contents")
	waitFor(function()
		return waiting
	end, "a callback to start waiting")

	watcher.stop()
	waitFor(function()
		return finished
	end, "a waiting callback to finish after its watcher stopped")
end)

-- Clean up, all watchers are stopped so the process is free to exit

fs.removeDir(ROOT)
