#!/usr/bin/env bash

#
# get-version-from-version-file.sh
#
# Get version from VERSION file.
#

VERSION=$(cat VERSION | tr -d '[:space:]')
echo "value=${VERSION}" >> "$GITHUB_OUTPUT"
