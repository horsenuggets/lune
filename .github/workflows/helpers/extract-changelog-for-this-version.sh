#!/usr/bin/env bash

#
# extract-changelog-for-this-version.sh
#
# Extract changelog for this version.
#

VERSION=$(cat VERSION | tr -d '[:space:]')
CHANGELOG=$(awk -v ver="\`$VERSION\`" '
  /^## / {
    if (found) exit
    if (index($0, ver) > 0) found=1
    next
  }
  found { print }
' CHANGELOG.md | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' | sed '/^$/N;/^\n$/d')

# Publish the multiline output; the release job writes its own notes file.
printf 'value<<EOF\n%s\nEOF\n' "$CHANGELOG" >> "$GITHUB_OUTPUT"
