# Frozen Krónan store states: provenance

Each `<state>.json` here is what the TypeScript Krónan package left on disk in one state that
`tests/ts/interop.test.ts` had the TypeScript side write and the binary read. After the TypeScript
package is deleted, these files are how `tests/money.rs` and `tests/persistence.rs` check that the
binary still reads what earlier TypeScript versions wrote. They are not regenerated.

- Source: `packages/kronan-mcp/src`, `packages/session-store/src` and `tests/ts/fixtures.ts` at
  41d6f98 (unchanged on `feat/ts-retire-1b`).
- Bun: 1.4.2, on macOS, with the store test seam (a file key, never the keychain).
- Generator, from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 bun tooling/ts-freeze/kronan-store.ts rust/kronan-mcp/tests/fixtures/store`
- Data: synthetic. The tokens are `synthetic-token-0123456789` and `synthetic-token-next-4242`.
  Each key is a random 32-byte key made for its scratch home; it protects only that synthetic
  token. Nothing reached Krónan or the real store.

| id `kronan/store/…` | case | the TypeScript calls |
| --- | --- | --- |
| `saved` | `interop.test.ts:40` | `saveToken(TOKEN)` on an empty home |
| `logged-out` | `interop.test.ts:40` | `saveToken(TOKEN)`, then `logoutToken()` |
| `migrated` | `interop.test.ts:65` | a legacy token file holding `NEXT`, then `migrateToken()` (result `migrated`) |
| `attempt-pickup-accepted` | `interop.test.ts:88` | `claimAttempt` for `reserve_pickup_slot`, checkout `checkout-token`, total 990, print `lines`, order `order-token` |
| `attempt-complete-accepted` | `interop.test.ts:116` | `claimAttempt` for `complete_checkout` on the fixture checkout, with its `fingerprint` |

Each file records:

- `result`: what the last TypeScript call returned.
- `print`: `fingerprint(checkoutSchema.parse(checkout))` for the fixture checkout.
- `files`: every file under the home, sorted. Each has its path, its permission bits (octal), and
  its bytes: `text` for `.json` files, `hex` otherwise.

Timestamps and attempt ids are as written; nothing is normalized. The binary prunes accepted
attempts older than 30 days only when it writes the record. The tests that plant one check that
the record is left byte for byte.
