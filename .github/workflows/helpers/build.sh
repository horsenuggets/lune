#!/usr/bin/env bash

#
# build.sh
#
# Build.
#

cargo build --workspace \
--locked --all-features \
--target "${WORKFLOW_MATRIX_CARGO_TARGET}"
