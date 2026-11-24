#!/usr/bin/env bash

#
# write-changelog-to-file.sh
#
# Write changelog to file.
#

printf '%s\n' "${WORKFLOW_NEEDS_INIT_OUTPUTS_CHANGELOG}" > /tmp/changelog.txt
