#!/usr/bin/env bash

#
# check-pr-title-format.sh
#
# Check PR title format.
#

echo "Checking PR title: $TITLE"

# Must be exactly "Release X.Y.Z" or "Release X.Y.Z-suffix"
if [[ ! "$TITLE" =~ ^Release\ [0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]; then
  echo "::error::PR title must be 'Release X.Y.Z' or 'Release X.Y.Z-suffix' (e.g., 'Release 0.10.4-horse.8.0')"
  exit 1
fi

echo "PR title is valid."
