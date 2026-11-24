#!/usr/bin/env bash

#
# check-version-bump.sh
#
# Check version bump.
#

git fetch origin release || true

VERSION=$(cat VERSION | tr -d '[:space:]')
echo "Current version: $VERSION"

# Check if this version tag already exists
if git rev-parse "$VERSION" >/dev/null 2>&1 || git rev-parse "v$VERSION" >/dev/null 2>&1; then
  echo "::error::Version $VERSION already has a tag. Please bump the version."
  exit 1
fi

echo "Version $VERSION is new."
