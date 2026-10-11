# Frozen store states: provenance

Each folder here is a complete store directory that the TypeScript `@family-mcp/session-store`
package wrote. `states.json` lists them with what that package then did with a copy. The Rust
tests in `tests/golden.rs` and `tests/persistence.rs` replay them. These files are the contract
once the TypeScript package is deleted; they are not regenerated.

- Source: `packages/session-store/src` at 41d6f98 (unchanged on `feat/ts-retire-freeze`).
- Bun: 1.4.2, on macOS (the `layout-macos` state needs macOS; the others do not).
- Generator, from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 bun tooling/ts-freeze/family-store.ts rust/family-store/tests/fixtures/store`
- Case ids: `created`, `one-write`, `three-writes`, `largest`, `pending-first`, `pending-later`,
  `pending-landed`, `pending-foreign`, `marker-behind`, `record-behind`, `lone-record`,
  `marker-only`, `layout-seam`, `layout-macos`, `layout-linux`.
- Data: synthetic. Every state has its own fresh random 32-byte key, made in a scratch directory
  only for this, and the values are made up. Nothing here protects a real secret.

## What each state records

- `files`: every file of the state, relative to the state folder (its store root, or its scratch
  HOME for the `layout-*` states).
- `modes`: the permissions the package gave each file (all `600`). Git keeps neither these nor the
  folder modes, so the tests copy a state into a scratch folder and set files to 0600 and folders
  to 0700 before use, as the package left them.
- `written`: the values the package wrote, in order.
- `typescript`: the package's own outcome on a copy: `read` (`value:<text>` or `error:<CODE>`),
  the marker and whether the record exists after that read, then `write` of `continued`, the
  marker after it, a `readAfterWrite`, and the files left.

## Normalization

None of the frozen bytes are normalized. Records carry the random nonce the package drew, so a
second run of the generator writes different bytes; these bytes are the frozen ones. The only
values the Rust tests do not compare byte for byte are records written during a test (by either
language), whose nonces are random: those are decrypted and their envelope is rebuilt from their
nonce instead (see `tests/golden.rs`).

The pending, behind and lone states are made by editing committed files exactly as
`tests/interop.rs` did: a marker rewritten with a `pending` member, an older marker or record put
back, or one file removed.
