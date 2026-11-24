#!/usr/bin/env bash

#
# test.sh
#
# Test.
#

cargo nextest run --workspace --no-fail-fast \
--locked --all-features \
--target "${WORKFLOW_MATRIX_CARGO_TARGET}"
