--[[

coverage_fixture

Shares source and exact native coverage contracts for local and actor execution.

--]]

const luau = require("@lune/luau")
const FIELD_COUNT = 700
const PHASES = {
	{ ChildHits = 0, RootHits = 0 },
	{ ChildHits = 0, RootHits = 1 },
	{ ChildHits = 1, RootHits = 1 },
	{ ChildHits = 2, RootHits = 1 },
}

type CoverageEntries = typeof(debug.getcoverage(function() end))
type Snapshot = {
	ChildHits: number,
	Entries: CoverageEntries,
	Level: number,
	RootHits: number,
}

const CoverageFixture = {}

function CoverageFixture.assertEntries(
	entries: CoverageEntries,
	level: number,
	rootHits: number,
	childHits: number
)
	assert(#entries == 2, "Expected the root and one nested prototype.")
	const root, child = entries[1], entries[2]
	assert(root.Function == "", "Expected an anonymous root prototype.")
	assert(root.LineDefined == 1 and root.Depth == 0)
	assert(child.Function == "makeTable", "Expected the nested prototype name.")
	assert(child.LineDefined == 1 and child.Depth == 1)
	assert(#root.Hits > FIELD_COUNT + 5)
	assert(#root.Hits == #child.Hits)

	for index, count in root.Hits do
		const line = index - 1
		const expected = if line == 1 or line == FIELD_COUNT + 5 then rootHits else -1
		assert(count == expected, `Expected root line {line} to have {expected} hits, got {count}.`)
	end
	for index, count in child.Hits do
		const line = index - 1
		const executable = line == 2 or (level == 2 and line >= 3 and line <= FIELD_COUNT + 2)
		const expected = if executable then childHits else -1
		assert(
			count == expected,
			`Expected child line {line} to have {expected} hits, got {count}.`
		)
	end
end

function CoverageFixture.assertSnapshots(snapshots: { Snapshot })
	assert(#snapshots == 2 * #PHASES, "Expected every phase at both coverage levels.")
	for level = 1, 2 do
		for phase, expected in PHASES do
			const snapshot = snapshots[(level - 1) * #PHASES + phase]
			assert(snapshot.Level == level)
			assert(snapshot.RootHits == expected.RootHits)
			assert(snapshot.ChildHits == expected.ChildHits)
			CoverageFixture.assertEntries(
				snapshot.Entries,
				level,
				expected.RootHits,
				expected.ChildHits
			)
		end
	end
end

function CoverageFixture.capture(): { Snapshot }
	assert(debug.iscoverageenabled(), "Coverage must be enabled for this fixture.")
	const lines = { "local function makeTable()", "return {" }
	for field = 1, FIELD_COUNT do
		table.insert(lines, `["Key{field}"] = true,`)
	end
	table.insert(lines, "}")
	table.insert(lines, "end")
	table.insert(lines, "return makeTable")
	const source = `{table.concat(lines, "\n")}\n`
	const snapshots = {}
	for level = 1, 2 do
		const bytecode = luau.compile(source, {
			coverageLevel = level,
			optimizationLevel = 1,
		})
		const chunk = luau.load(bytecode, { codegenEnabled = false })
		const function record(rootHits: number, childHits: number)
			const entries = debug.getcoverage(chunk)
			CoverageFixture.assertEntries(entries, level, rootHits, childHits)
			table.insert(snapshots, {
				ChildHits = childHits,
				Entries = entries,
				Level = level,
				RootHits = rootHits,
			})
		end
		record(0, 0)
		const child = chunk()
		record(1, 0)
		for hits = 1, 2 do
			const result = child()
			assert(result.Key700 == true)
			record(1, hits)
		end
	end
	return snapshots
end

return CoverageFixture
