#!/usr/bin/env bash

#
# check-diff-with-main.sh
#
# Check diff with main.
#

git fetch origin main

# Get the diff between the PR branch and main
DIFF=$(git diff origin/main..HEAD)

if [ -n "$DIFF" ]; then
  echo "::error::PR branch has changes that differ from main. The release branch must contain exactly what is in main."
  echo "Diff:"
  echo "$DIFF"
  exit 1
fi

echo "PR branch matches main exactly."
