#!/usr/bin/env bash

#
# check-changelog-entry.sh
#
# Check changelog entry.
#

VERSION=$(cat VERSION | tr -d '[:space:]')

# Check if changelog has an entry for this version
if ! grep -q "## \`$VERSION\`" CHANGELOG.md; then
  echo "::error::CHANGELOG.md does not have an entry for version $VERSION"
  echo "Expected to find: ## \`$VERSION\`"
  exit 1
fi

echo "Changelog entry found for version $VERSION."
