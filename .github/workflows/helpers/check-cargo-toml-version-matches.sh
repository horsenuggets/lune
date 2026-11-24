#!/usr/bin/env bash

#
# check-cargo-toml-version-matches.sh
#
# Check Cargo.toml version matches.
#

VERSION=$(cat VERSION | tr -d '[:space:]')
CARGO_VERSION=$(./scripts/get-version.sh)

if [ "$VERSION" != "$CARGO_VERSION" ]; then
  echo "::error::VERSION file ($VERSION) does not match Cargo.toml ($CARGO_VERSION)"
  exit 1
fi

echo "VERSION file matches Cargo.toml."
