#!/usr/bin/env bash

#
# create-release.sh
#
# Create release.
#

# Tag the commit that built these artifacts, even if the default branch has moved.
gh release create "${WORKFLOW_NEEDS_INIT_OUTPUTS_VERSION:?The release version must be provided.}" \
  --title "${WORKFLOW_NEEDS_INIT_OUTPUTS_VERSION}" \
  --target "${GITHUB_SHA:?The release commit must be provided.}" \
  --notes-file /tmp/changelog.txt \
  ./releases/*.zip
