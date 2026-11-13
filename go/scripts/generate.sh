#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
tools_dir=$(mktemp -d)
trap 'rm -rf "$tools_dir"' EXIT HUP INT TERM
GOBIN="$tools_dir" go install google.golang.org/protobuf/cmd/protoc-gen-go@v1.36.6
GOBIN="$tools_dir" go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@v1.5.1
export PATH="$tools_dir:$PATH"
schema_dir=../crates/wx-ime-protocol/proto
protoc -I "$schema_dir" \
  --go_out=internal/wire --go_opt=paths=source_relative \
  --go_opt=Mwetype.proto=github.com/mkdir700/wx-ime-sdk/go/internal/wire \
  "$schema_dir/wetype.proto"
protoc -I "$schema_dir" \
  --go_out=internal/flurry --go_opt=paths=source_relative \
  --go_opt=Mflurry.proto=github.com/mkdir700/wx-ime-sdk/go/internal/flurry \
  --go-grpc_out=internal/flurry --go-grpc_opt=paths=source_relative \
  --go-grpc_opt=Mflurry.proto=github.com/mkdir700/wx-ime-sdk/go/internal/flurry \
  "$schema_dir/flurry.proto"
