#!/usr/bin/env bash

#
# verify-pr-title-matches-version.sh
#
# Verify PR title matches VERSION.
#

VERSION=$(cat VERSION | tr -d '[:space:]')
EXPECTED_TITLE="Release $VERSION"

if [ "$EXPECTED_TITLE" != "$ACTUAL_TITLE" ]; then
  echo "::error::PR title '$ACTUAL_TITLE' does not match VERSION file. Expected '$EXPECTED_TITLE'"
  exit 1
fi

echo "PR title matches VERSION file."
