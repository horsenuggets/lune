#!/usr/bin/env bash

#
# build-binary.sh
#
# Build binary.
#

cargo build \
--locked --all-features \
--release --target "${WORKFLOW_MATRIX_CARGO_TARGET}"
