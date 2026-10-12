# Release process

Source: https://github.com/olafurns7/family-mcp/tree/main/packages/abler-mcp
Tags use `abler-mcp@<version>`; the current version is `abler-mcp@0.7.2`.
Native binaries are the only distribution.

1. Bump this package's `version` in `package.json`.
2. From the repository root, run `bun run release:sync`.
3. Commit the version and synchronized files.
4. Create the matching `abler-mcp@<version>` tag.
5. Push the tag.
6. Wait for the draft release to appear.
7. Verify its four native archives, SHA-256 files, and `install.sh` asset.
8. Publish the draft release.

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/abler-mcp@0.7.2/packages/abler-mcp/install.sh | sh
```

Archives contain the executable, README, LICENSE, and generated third-party
notices. The installer verifies the checksum and version before replacing the
command; previous version directories remain. Never include sessions,
credentials, environment files, browser captures, or fixtures in release assets.
