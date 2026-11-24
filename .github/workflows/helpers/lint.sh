#!/usr/bin/env bash

#
# lint.sh
#
# Lint.
#

cargo clippy --workspace \
--locked --all-features \
--target "${WORKFLOW_MATRIX_CARGO_TARGET}"
