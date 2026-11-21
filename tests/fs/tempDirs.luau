--[[

tempDirs

Checks native temporary-directory isolation, concurrent creation, argument validation,
and explicit caller cleanup without changing existing sibling files.

--]]

const fs = require("@lune/fs")
const task = require("@lune/task")
const ROOT = "bin/fs_temp_dirs_test"
fs.writeDir(ROOT)
const paths: { string } = {}
const success, problem = pcall(function()
	const sibling = `{ROOT}/draft-existing`
	fs.writeFile(sibling, "Keep this sibling.")
	local completed = 0
	const errors: { string } = {}
	for _ = 1, 32 do
		task.spawn(function()
			const ok, result = pcall(fs.createTempDir, ROOT, "draft-")
			if ok then
				table.insert(paths, result)
			else
				table.insert(errors, tostring(result))
			end
			completed += 1
		end)
	end
	while completed < 32 do
		task.wait()
	end
	assert(#errors == 0, table.concat(errors, "\n"))
	const seen: { [string]: boolean } = {}
	for _, path in paths do
		assert(not seen[path], "Temporary directories must be unique.")
		seen[path] = true
		assert(fs.isDir(path), "Temporary directories must survive the creation call.")
		const file = `{path}/Place.rbxl`
		fs.writeFile(file, "Place contents.")
		assert(
			fs.readFile(file) == "Place contents.",
			"The caller must be able to use the directory."
		)
		fs.removeDir(path)
		assert(not fs.isDir(path), "The caller must be able to remove the directory.")
	end
	assert(fs.readFile(sibling) == "Keep this sibling.", "Existing siblings must remain intact.")
	for _, prefix in
		{
			"",
			".",
			"..",
			"../outside",
			"nested/path",
			"nested\\path",
			"C:drive",
			"\\\\host",
			"bad\0prefix",
		}
	do
		assert(not pcall(fs.createTempDir, ROOT, prefix), "Invalid prefixes must be rejected.")
	end
	assert(
		not pcall(fs.createTempDir, `{ROOT}/missing/parent`, "draft-"),
		"A missing parent must fail."
	)
	assert(
		not pcall(fs.createTempDir, sibling, "draft-"),
		"A file cannot be a temporary directory's parent."
	)
	assert(not pcall(fs.createTempDir, "", "draft-"), "The parent must be explicit.")
	assert(not pcall(fs.createTempDir :: any, ROOT), "The prefix must be explicit.")
end)
fs.removeDir(ROOT)
assert(success, problem)
