#!/usr/bin/env bash

set -euo pipefail

styluau .lune crates scripts tests \
	--glob "tests/**/*.luau" \
	--glob "!tests/roblox/rbx-test-files/**"

cargo fmt
