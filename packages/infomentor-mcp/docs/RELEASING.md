# Release process

Source: https://github.com/olafurns7/family-mcp/tree/main/packages/infomentor-mcp
Tags use `infomentor-mcp@<version>`; the current version is `infomentor-mcp@0.9.2`.
Native binaries are the only distribution.

1. Bump this package's `version` in `package.json`.
2. From the repository root, run `bun run release:sync`.
3. Commit the version and synchronized files.
4. Create the matching `infomentor-mcp@<version>` tag.
5. Push the tag.
6. Wait for the draft release to appear.
7. Verify its four native archives, SHA-256 files, and `install.sh` asset.
8. Publish the draft release.

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/infomentor-mcp@0.9.2/packages/infomentor-mcp/install.sh | sh
```

The executable is compiled from the Rust crate `rust/infomentor-mcp`
(`familyMcp.release.rust` in `package.json`). `release:sync` also sets the crate
version in `rust/infomentor-mcp/Cargo.toml` and `rust/Cargo.lock`, and
`release:check` fails while they differ. Before tagging, run the Rust checks in
the package README and
`bunx turbo run build:binary test:binary test:installer --filter=infomentor-mcp --force`.

Archives contain the executable, README, LICENSE, and generated third-party
notices, which list every statically linked crate. The installer verifies the checksum and version before replacing the
command; previous version directories remain. Never include sessions,
credentials, environment files, browser captures, or fixtures in release assets.

Linux archives include the optional `direct-route.py` helper. `--with-direct-route`
checks the alternate frontend before adding its managed hosts entry; upgrades
revalidate it and `--without-direct-route` removes it. Its offline Python check
runs with the package tests. Verify the real helper and authenticated MCP reads
on an authorized Linux host before releasing changes to this integration.

Linux archives also include the optional WARP installer and launcher. `--with-warp`
is explicit opt-in; ordinary upgrades preserve the chosen network mode and
`--without-warp` restores direct access. CI substitutes administrator commands.
Real Debian 13/x64 WARP installation and daemon-recovery verification remain
required before releasing changes to that integration.
