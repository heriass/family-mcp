# Release process

The current preview is `dominos-mcp@0.2.1`. Native binaries are the only
distribution. The executable is now built from `rust/dominos-mcp`, using the
pinned Rust compiler, locked dependencies and rustls, with default features only.
Bun runs the retained TypeScript reference and offline parity fixtures; it is not
needed on the installed host. Never enable `test-origin` in a release build.

The earlier TypeScript preview had live SMS login, refresh, quote and saved-card
session checks. The Rust port has only synthetic loopback validation. Charging
remains untested and bank-verification continuation is not implemented; retain
these limitations in the release notes.

From the repository root:

```sh
bun run --cwd packages/dominos-mcp release:sync
bunx turbo run check test release:check --filter=dominos-mcp
bunx turbo run build:binary test:binary test:installer --filter=dominos-mcp --force
```

Run the Rust fmt, Clippy and all-feature tests documented in the package README
before the native build. Run binary and installer checks under a scratch HOME;
they must not access the maintainer’s store or installed command. The package
README and this guide are already included in release version synchronization.

The package uses the shared native build, checksum-verifying installer, and
`dominos-mcp@<version>` release workflow. The maintainer must authorize version
changes, commits, tags, pushes, and publication. The workflow produces a draft
prerelease with macOS/Linux arm64/x64 archives, SHA-256 files, and `install.sh`.

After the checks pass, commit the release files, create the matching tag, and push
it. Wait for the draft release, verify all four archives, checksums, and installer,
then publish it as a prerelease.

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/dominos-mcp@0.2.1/packages/dominos-mcp/install.sh | sh
```

Archives contain the executable, README, LICENSE, and generated dependency
notices. Never include sessions, checkout records, environment files, SMS codes,
card tokens, browser captures, or test fixtures. Offline binary and installer
checks are not evidence of live account connectivity or a successful purchase.
