# CLI build and npm publishing

The Rust workspace version is the single version source. Supported versions are
`X.Y.Z`, `X.Y.Z-alpha.N`, `X.Y.Z-beta.N`, and `X.Y.Z-rc.N`. Stable publishes to
`latest`; prereleases publish to their channel. Update Cargo.toml (including local
crate dependency versions) and Cargo.lock together before tagging.

This follows Desktop's CLI distribution layout: one launcher and five exact-version
optional platform packages, with the platform packages published first. It does
not publish the Rust SDK as a JavaScript API. No postinstall script is needed.
The project is licensed under Apache-2.0. Every generated package includes LICENSE.
Third-party dependencies retain their own licenses; preserve their required notices
when distributing native binaries.

## Repository and first npm publication

1. Create `mkdir700/wx-ime-sdk` on GitHub and push the reviewed changes. The local
   checkout initially had no remote; these changes do not create or publish a repository.
2. Allow GitHub Actions; configure environment `npm` with the desired release
   approvers. Do not configure a long-lived npm token.
3. Run the build workflow and download its `npm-*` tarball artifacts. For the first
   publication, authenticate locally with an npm account permitted to publish under
   `@mkdir700` (complete the npm interactive 2FA prompt). Publish the five platform
   tarballs first, then the launcher tarball, using `npm publish FILE --access public
   --tag TAG`. Do not publish placeholders: these should be the actual verified CLI.
4. Configure a GitHub Actions trusted publisher for **each** package:
   owner `mkdir700`, repository `wx-ime-sdk`, workflow `npm-publish.yml`,
   environment `npm`. See https://docs.npmjs.com/trusted-publishers/.
   Packages: `@mkdir700/wxc`, plus `@mkdir700/wxc-{darwin-arm64,darwin-x64,
   linux-arm64,linux-x64,win32-x64}` (five separate names, not brace syntax).

## Subsequent releases

Publish a GitHub Release for `v<workspace version>` after reviewing the source.
Draft releases never publish npm packages. The workflow resolves the release tag
once to a commit SHA, checks the Cargo version, rebuilds all platform packages,
and runs installation smoke checks on their native runners. Only after every
build passes can the npm environment publish using OIDC and provenance.

Manual `workflow_dispatch` takes an **existing published** release tag; it does not
publish arbitrary branch builds. If another workflow creates the release with
GITHUB_TOKEN, its event will not start a second workflow: dispatch npm-publish
explicitly for that published tag. A retry skips already-published versions only
when the tarball integrity matches exactly. A mismatch fails: investigate it,
retain the original release artifacts or issue a new version; never overwrite.

## Local packaging verification

```sh
cargo build --locked -p wxc --features vendored-openssl
node scripts/npm-package.mjs build darwin-arm64 target/debug/wxc
node scripts/npm-smoke.mjs
```

Replace the platform and binary path for your machine. The smoke check packs both
packages, installs the tarballs offline with scripts disabled, runs the installed
`wxc` shim, and checks help/version/error exit codes and version mismatch rejection.
Evidence is `npm-smoke/results.json`; packed tarballs are in the same directory.
It does not pair with WeChat, access the network, or touch an existing identity.
CI repeats this with newly built release binaries on every supported platform.
Linux packages target glibc 2.35+; Alpine/musl is not supported.
