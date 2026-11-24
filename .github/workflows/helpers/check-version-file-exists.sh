#!/usr/bin/env bash

#
# check-version-file-exists.sh
#
# Check version file exists.
#

if [ ! -f "VERSION" ]; then
  echo "::error::VERSION file not found"
  exit 1
fi
echo "VERSION file exists."
