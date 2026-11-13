#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
fixture_file=$(mktemp)
trap 'rm -f "$fixture_file"' EXIT HUP INT TERM
cargo run --quiet --locked --manifest-path interop/rust/Cargo.toml > "$fixture_file"
cp "$fixture_file" testdata/rust-protocol.json
