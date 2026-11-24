#!/usr/bin/env bash

#
# test-helpers.sh
#
# Checks helper syntax, release inputs and command arguments without remote services.
# Run with "bash .github/workflows/helpers/test-helpers.sh".
#

set -euo pipefail

HELPERS=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
SANDBOX=$(mktemp -d)
trap 'rm -rf -- "$SANDBOX"' EXIT
printf '1.2.3\n' > "$SANDBOX/VERSION"

for helper in "$HELPERS"/*.sh; do
  bash -n "$helper"
done

cd -- "$SANDBOX"

env 'TITLE=Release 1.2.3-horse.0.2' bash -e "$HELPERS/check-pr-title-format.sh" >/dev/null
if env TITLE=not-a-release bash -e "$HELPERS/check-pr-title-format.sh" >/dev/null 2>&1; then
  printf 'Expected "check-pr-title-format.sh" to reject invalid input.\n' >&2
  exit 1
fi

env 'ACTUAL_TITLE=Release 1.2.3' bash -e "$HELPERS/verify-pr-title-matches-version.sh" >/dev/null
if env 'ACTUAL_TITLE=Release 9.9.9' bash -e "$HELPERS/verify-pr-title-matches-version.sh" >/dev/null 2>&1; then
  printf 'Expected "verify-pr-title-matches-version.sh" to reject invalid input.\n' >&2
  exit 1
fi

# GitHub output paths may contain spaces, and changelog output must stay multiline.
export GITHUB_OUTPUT="$SANDBOX/outputs with spaces"
bash -e "$HELPERS/get-version-from-version-file.sh"
[[ $(< "$GITHUB_OUTPUT") == 'value=1.2.3' ]]
printf '%s\n' '# Changelog' '' "## \`1.2.3\`" '' '- Included.' '' "## \`1.2.2\`" '' '- Excluded.' > CHANGELOG.md
: > "$GITHUB_OUTPUT"
bash -e "$HELPERS/extract-changelog-for-this-version.sh"
OUTPUT=$(< "$GITHUB_OUTPUT")
[[ "$OUTPUT" == "value<<EOF"$'\n'* ]]
[[ "$OUTPUT" == *'- Included.'* ]]
[[ "$OUTPUT" != *'- Excluded.'* ]]
[[ "$OUTPUT" == *$'\nEOF' ]]

# Stub executable boundaries rather than contacting GitHub or starting Cargo builds.
export CAPTURE="$SANDBOX/arguments"
function cargo() { printf '%s\n' "$@" > "$CAPTURE"; }
function gh() { printf '%s\n' "$@" > "$CAPTURE"; }
function git() {
  case "$1" in
    fetch) return 0 ;;
    rev-parse) [[ "$2" == "$EXISTING_TAG" ]] ;;
    *) return 2 ;;
  esac
}
export -f cargo gh git

env WORKFLOW_MATRIX_CARGO_TARGET=x86_64-unknown-linux-gnu \
  bash --noprofile --norc -e -o pipefail "$HELPERS/test.sh"
EXPECTED=$(printf '%s\n' nextest run --workspace --no-fail-fast --locked --all-features \
  --target x86_64-unknown-linux-gnu)
[[ $(< "$CAPTURE") == "$EXPECTED" ]]

for EXISTING_TAG in 1.2.3 v1.2.3; do
  if env EXISTING_TAG="$EXISTING_TAG" bash -e "$HELPERS/check-version-bump.sh" >/dev/null 2>&1; then
    printf 'Expected "check-version-bump.sh" to reject existing tag "%s".\n' "$EXISTING_TAG" >&2
    exit 1
  fi
done
env EXISTING_TAG=unrelated bash -e "$HELPERS/check-version-bump.sh" >/dev/null

mkdir releases
printf '' > 'releases/archive with spaces.zip'
RELEASE_COMMIT=0123456789abcdef0123456789abcdef01234567
env WORKFLOW_NEEDS_INIT_OUTPUTS_VERSION=1.2.3 GITHUB_SHA="$RELEASE_COMMIT" \
  bash --noprofile --norc -e -o pipefail "$HELPERS/create-release.sh"
EXPECTED=$(printf '%s\n' release create 1.2.3 --title 1.2.3 --target "$RELEASE_COMMIT" \
  --notes-file /tmp/changelog.txt './releases/archive with spaces.zip')
[[ $(< "$CAPTURE") == "$EXPECTED" ]]
for missing in GITHUB_SHA WORKFLOW_NEEDS_INIT_OUTPUTS_VERSION; do
  if (
    export GITHUB_SHA="$RELEASE_COMMIT" WORKFLOW_NEEDS_INIT_OUTPUTS_VERSION=1.2.3
    unset "$missing"
    bash -e "$HELPERS/create-release.sh"
  ) >/dev/null 2>&1; then
    printf 'Expected "create-release.sh" to reject missing input "%s".\n' "$missing" >&2
    exit 1
  fi
done

printf 'Workflow helper checks passed.\n'
