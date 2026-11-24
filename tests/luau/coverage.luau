--[[

coverage

Checks exact native coverage metadata, executable masks and counts through public APIs.

--]]

const CoverageFixture = require("./coverage_fixture")

CoverageFixture.assertSnapshots(CoverageFixture.capture())
