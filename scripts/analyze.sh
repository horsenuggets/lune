#!/usr/bin/env bash

set -euo pipefail

lune run scripts/analyze_copy_typedefs

luau-lsp analyze \
	--platform=standard \
	--settings=".vscode/settings.json" \
	--ignore="tests/roblox/rbx-test-files/**" \
	--ignore="tests/wally_test/**" \
	--ignore="tests/require/project_test/**" \
	--ignore="tests/require/script_ref.luau" \
	--ignore="tests/require/script_ref_module.luau" \
	--ignore="tests/globals/script.luau" \
	.lune crates scripts tests
