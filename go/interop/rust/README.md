# Rust protocol fixture generator

This standalone development crate generates `../../testdata/rust-protocol.json`
from the repository's Rust protocol implementation. The generator constructs
public test scalars 1 and 2 through OpenSSL at runtime. No serialized private key
is committed. Never use these deterministic test identities for real devices.

From the Go module directory:

```sh
sh scripts/rust-fixture.sh
```

The output is deterministic. It includes a Rust-format identity template, a bootstrap-key
encrypted ECDH response, and a raw-Snappy-compressed, session-key-encrypted
protobuf group/clipboard response. `TestRustFixtureE2E` drives the full Go client
against these response bytes through a real local HTTP server and persistent
state directory. The Go E2E fills the template's empty `private_pem` field with
an in-memory PKCS#8 encoding of test scalar 1.

This crate requires the same Rust, OpenSSL, C compiler, and `protoc` toolchain as
`wx-ime-protocol`. Normal Go builds and E2E runs use the committed fixture and do
not require Rust.
