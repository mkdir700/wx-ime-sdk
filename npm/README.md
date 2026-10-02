# wxc

WeChat IME (WeType) clipboard CLI. This is a native Rust CLI, not a JavaScript SDK.

```sh
npm install -g @mkdir700/wxc
wxc --help
wxc --version
# Or without a global install:
npm exec --package=@mkdir700/wxc -- wxc --help
```

Node.js 22+ is required for the launcher. Prebuilt packages support macOS arm64/x64,
Linux glibc 2.35+ arm64/x64, and Windows x64. OpenSSL is compiled into the binary.
No Rust compiler, install scripts, or external binary downloads are required.
Keep npm optional dependencies enabled. Alpine/musl and Windows arm64 are not supported.

Use only your own devices and accounts. See the repository README for pairing,
private identity storage, clipboard delivery semantics, and known limitations.

## License and disclaimer

Licensed under Apache-2.0; see LICENSE. Third-party components retain their own
licenses. This is an independent, unofficial implementation, not endorsed by or
affiliated with Tencent or WeChat. Product names and trademarks belong to their
respective owners. Compatibility, service availability and delivery are not
guaranteed; protocol or policy changes may break functionality or restrict accounts.
Use only devices, accounts and data you own or are authorized to access, and comply
with applicable laws and service terms. Protect private keys, tokens and clipboard
data. The software is provided AS IS, subject to the warranty disclaimer and
liability limits in LICENSE; no non-excludable legal rights or liabilities are waived.
The software license does not grant access rights to third-party services.
