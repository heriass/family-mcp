# Release process

The current preview is `inna-mcp@0.4.2`. Native binaries are the only
distribution. The initial 0.1.0 phone-prompt electronic-ID login, private session
reuse, and all 13 read/status tools were checked live in an owner-authorized
guardian account. The 0.1.1 parsing and polling changes are checked offline.
Whole-day illness and leave creation are source verified and tested offline;
no real absence was submitted. Keep that limit in the preview release notes.

From the repository root:

```sh
bun run --cwd packages/inna-mcp release:sync
bunx turbo run check test release:check --filter=inna-mcp
bunx turbo run test:binary test:installer --filter=inna-mcp
```

The executable is compiled from the Rust crate `rust/inna-mcp`
(`familyMcp.release.rust` in `package.json`). `release:sync` also sets the crate
version in `rust/inna-mcp/Cargo.toml` and `rust/Cargo.lock`, and
`release:check` fails while they differ. Before tagging, run the Rust checks in
the package README and
`bunx turbo run build:binary test:binary test:installer --filter=inna-mcp --force`.

The package uses the shared native build, checksum-verifying installer, and
`inna-mcp@<version>` release workflow. The maintainer must authorize version
changes, commits, tags, pushes, and publication. The workflow produces a draft
prerelease with macOS/Linux arm64/x64 archives, SHA-256 files, and `install.sh`.

After authorized publication, install with:

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/inna-mcp@0.4.2/packages/inna-mcp/install.sh | sh
```

Verify all four workflow-built archives and checksums before publishing the
draft prerelease. A local macOS check establishes only that platform's native
and installer behavior. CI must remain offline and never send phone requests or
create school records.

Archives contain the executable, README, LICENSE, and generated third-party
notices, which list every statically linked crate. Never include sessions, absence markers, phone
numbers, identity numbers, tokens, browser captures, environment files, or test
fixtures. Agents running login must immediately show the user's comparison
code, including leading zeros; the PIN stays on the phone.
