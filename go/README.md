# WeType Go SDK

An independent Go module that ports the Rust SDK in this repository. It provides
persistent device identities, both pairing roles, device discovery, text and image
clipboard exchange, and authenticated LAN file transfer. It never accesses the
host operating system clipboard.

```go
import wxime "github.com/mkdir700/wx-ime-sdk/go"
```

## Requirements

- Go 1.24 or later with `CGO_ENABLED=1`.
- A C compiler, `pkg-config`, and OpenSSL development files with secp128r1 support.
- An authorized WeType peer for official-service use.

OpenSSL supplies the same secp128r1 ECDH backend used by Rust. Go's standard TLS
and X.509 implementations supply the separate ephemeral P-256 file identities.
The module does not execute the Rust CLI or require Rust at runtime.

On Debian/Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libssl-dev
```

On macOS:

```sh
brew install openssl@3 pkg-config
export PKG_CONFIG_PATH="$(brew --prefix openssl@3)/lib/pkgconfig"
```

Generated protobuf and gRPC bindings are committed. Consumers do not need
`protoc`. A Windows build also requires a compatible C toolchain and OpenSSL
`pkg-config` metadata; Windows has not been verified in this change.

## Add to another service

After a revision containing this directory has been pushed, pin that revision:

```sh
go get github.com/mkdir700/wx-ime-sdk/go@REVISION
```

For local development, use a Go module replacement in the consuming service:

```sh
go mod edit -require=github.com/mkdir700/wx-ime-sdk/go@v0.0.0
go mod edit -replace=github.com/mkdir700/wx-ime-sdk/go=/absolute/path/to/wx-ime-sdk/go
go mod tidy
```

A release tag for this nested module must use the `go/vX.Y.Z` prefix. Creating
this module does not publish a release.

## Open, pair, and send

```go
ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
defer cancel()

client, err := wxime.Open(ctx, wxime.ClientOptions{
    StateDir:   "/var/lib/my-service/wetype",
    DeviceName: "My service",
})
if err != nil {
    return err
}
defer client.Close()

code, err := wxime.ParsePairingCode("012345")
if err != nil {
    return err
}
pairing, err := client.Join(ctx, code)
if err != nil {
    return err
}
defer pairing.Close()
if _, _, err = pairing.Wait(ctx); err != nil {
    return err
}
pairing.Close()

receipt, err := client.SendText(ctx, "Hello from my service")
if err != nil {
    return err
}
_ = receipt // Server acceptance only; this is not destination delivery proof.
```

Pair only when creating an unpaired identity. Reopen the same directory and name
to reuse its identity. An already paired identity may reject another join. To
create another identity, choose another private state directory.

To issue a code, call `RequestPairingCode`, display `Code().Expose()`, and call
`NextUpdate`. When it returns `ConfirmationRequired`, inspect `Peer` and call
`Confirm` only after host approval. `Wait` returns `ErrConfirmationRequired` for
this state and never confirms automatically. `Cancel` releases the remote
transaction and verifies its state. `Close` releases only the local pairing
lease. Pairing codes preserve leading zeros and are redacted when formatted.

A runnable integration example is included:

```sh
cd go
go run ./examples/service -state /private/path/wetype -name 'My service' \
  -pair 012345 -text 'Hello'
# Later calls reuse the identity and omit -pair.
go run ./examples/service -state /private/path/wetype -name 'My service' \
  -text 'Another value'
```

## Clipboard receive and acknowledgment

- `SendText`: 1 MiB maximum UTF-8; up to 256 KiB inline, larger text through COS.
- `SendImage`: an encoded file of up to 16 MiB; AES-128-CTR object encryption and
  plaintext MD5 verification. The SDK does not transcode images.
- `PollClipboardOnce`: one poll of the latest value, or `nil` when no new receipt
  is ready. An emitted receipt is not emitted again in the same client instance.
- `NextEvent`: polls until `GroupChanged` or `ClipboardReceived`. Cancel its
  context to stop waiting. The poll interval is two seconds.
- `InspectClipboard`: read-only latest-value metadata and skip reasons.
- `Acknowledge`: commit the exact receipt ID after the host commits the content.

These APIs poll the latest value. They are not a history queue. Only one receipt
can remain unacknowledged. Its content and ID are persisted before delivery to
the host. Restarting replays that receipt. Copy a received image from
`Content.Image.Path` before acknowledgment; acknowledgment reclaims that cache.

```go
received, err := client.PollClipboardOnce(ctx)
if err != nil {
    return err
}
if received != nil {
    // Commit received.Content to your service before acknowledging it.
    if err := commitToService(received); err != nil {
        return err
    }
    if err := client.Acknowledge(ctx, received.ID); err != nil {
        return err
    }
}
```

`commitToService` is the consuming service's durable storage operation. It is not
an SDK function. Deduplicate that operation by `ReceivedID` so a crash between
host commit and acknowledgment does not apply content twice.

If `errors.Is(err, wxime.ErrOutcomeUnknown)` is true, the server may have accepted
publication. The SDK does not automatically retry publication. Check the peer
before deciding whether to retry. A successful `SendReceipt` proves only server
acceptance.

## LAN files

```go
ips := []net.IP{net.ParseIP("192.168.1.20")}
transfer, err := client.SendFile(ctx, peerUin, "/data/report.pdf", ips)
if err != nil {
    return err
}
defer transfer.Cancel()
receipt, err := transfer.Wait(ctx)
```

`Progress` and `Updates` report acknowledged upload bytes. `Wait` completes only
after the receiver acknowledges the complete byte range. The file ID is its
BLAKE3 digest. The sender snapshots the source into a private temporary file so
later source writes cannot alter transmitted bytes. Files are limited to 1 GiB.
The temporary snapshot is removed on completion or cancellation.

The receiving service queries `PendingFileInvite`, deduplicates by
`invite.Code.Expose()`, and accepts an invitation from its paired sender:

```go
receiver, err := client.ReceiveFile(ctx, invite.SenderUin, invite.Code,
    wxime.FileReceiveOptions{
        BindAddress:   "0.0.0.0:0",
        AdvertisedIPs: []net.IP{net.ParseIP("192.168.1.21")},
        DownloadDir:   "/var/lib/my-service/incoming",
    })
if err != nil {
    return err
}
defer receiver.Close()
file, err := receiver.Receive(ctx)
```

Native controls continue while the host is idle. One completed file can wait for
the host. Another file is rejected until the host takes it. Completed files stay
readable after peer disconnection. A corrupted or partial file is removed. Each
completed file is stored in its own private directory, so peer filenames cannot
overwrite another file. Move completed files into host storage and remove their
empty private directories when appropriate.

The transfer uses mutually authenticated TLS over a direct LAN gRPC connection.
The SDK has no WAN/NAT relay and does not discover local addresses. Supply
reachable addresses and permit the listening port in the host firewall.
`PendingFileInvite.SendTime` retains the raw server value; its time unit remains
unverified. Official application file-target advertising has the same unresolved
compatibility limitation documented by the Rust SDK.

## Lifetimes and persistent state

`Client` is safe for concurrent use. All network operations accept contexts.
`Close` cancels active work, waits for file workers, and releases the identity
lock. Call `Cancel`/`Close` on owned handles; Go garbage collection is not an
operation cancellation mechanism. A canceled `Wait` context stops that wait;
call `FileTransfer.Cancel` to cancel the transfer itself.

One state directory permits one live client across processes. Identity JSON,
PKCS#8 PEM, device-code checksum, consumed cursors, and tagged clipboard content
use the Rust format. Pending image paths are absolute. Keep the directory at the
same location when changing language implementations. Never open the same
identity concurrently from Rust and Go.

`CaptureDir` is opt-in and must not already exist. It stores private control
headers, ciphertext, decrypted responses, and object evidence. These files can
contain credentials and clipboard content. Keep them private. Normal operation
creates no capture directory and does not log clipboard content or secrets.
Go garbage collection does not guarantee erasure of every copied secret.

`HTTPClient` optionally supplies a service-owned HTTP transport. Redirects remain
disabled. The default transport uses direct networking and ignores environment
proxy variables, as the Rust implementation does. COS URLs must use HTTPS on
port 443 and a `.myqcloud.com` host.

## Verification

```sh
cd go
go vet ./...
WXIME_E2E_ARTIFACT=artifacts/e2e.json go test -race -count=1 -v ./...
```

The E2E scenario uses an HTTP protocol emulator, encrypted protobuf envelopes,
raw Snappy responses, COS object IO, and real loopback TLS gRPC sockets. It covers
pairing confirmation/cancellation, persistent locks and receipts, text and image
exchange, uncertain publication, multi-block and empty files, idle controls,
queue rejection, untrusted certificates, digest failures, cleanup, cancellation,
and shutdown. It does not contact the official service or operate a GUI.

`TestRustFixtureE2E` also opens an identity produced by Rust and consumes encrypted,
compressed protobuf responses produced by the actual Rust protocol library. It
acknowledges the Rust clipboard value and verifies the cursor after reopening.
The fixture generator is in `interop/rust`; it constructs public test scalars at
runtime and commits no serialized private keys.
Regenerate the committed fixture with `sh scripts/rust-fixture.sh` when the Rust
protocol changes. That optional development step requires the Rust toolchain.

`artifacts/e2e.json` and `artifacts/rust-interop.json` are deterministic evidence
with content digests and completed checks. Regenerating them must produce the
same bytes. CI runs the E2E scenarios with
race detection on Linux and macOS and uploads this artifact. Adding a workflow
does not establish that its remote run has passed.

Regenerate protocol bindings from the same schemas used by Rust:

```sh
sh scripts/generate.sh
```

This development step needs `protoc` and downloads pinned Go generator versions.

## License and compatibility

Apache-2.0; see [LICENSE](LICENSE). This is an unofficial protocol compatibility
implementation. The upstream service may change its protocol or availability.
The [repository notice](../README.md) applies to authorized devices, accounts,
and data. Official-device acceptance has not been run for this Go port.
