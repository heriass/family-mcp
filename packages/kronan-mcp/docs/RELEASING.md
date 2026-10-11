# Release process

Source: https://github.com/olafurns7/family-mcp/tree/main/packages/kronan-mcp
Tags use `kronan-mcp@<version>`; the current version is `kronan-mcp@0.3.1`.
Native binaries are the only distribution.

1. Bump this package's `version` in `package.json`.
2. From the repository root, run `bun run release:sync`.
3. Commit the version and synchronized files.
4. Create the matching `kronan-mcp@<version>` tag.
5. Push the tag.
6. Wait for the draft release to appear.
7. Verify its four native archives, SHA-256 files, and `install.sh` asset.
8. Publish the draft release.

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/kronan-mcp@0.3.1/packages/kronan-mcp/install.sh | sh
```

The executable is compiled from the Rust crate `rust/kronan-mcp`
(`familyMcp.release.rust` in `package.json`). `release:sync` also sets the crate
version in `rust/kronan-mcp/Cargo.toml` and `rust/Cargo.lock`, and
`release:check` fails while they differ. Before tagging, run the Rust checks in
the package README and
`bunx turbo run build:binary test:binary test:installer --filter=kronan-mcp --force`.

Archives contain the executable, README, LICENSE, and generated third-party
notices, which list every statically linked crate. The installer verifies the checksum and version before replacing the
command; previous version directories remain. Never include sessions,
credentials, environment files, browser captures, or fixtures in release assets.
