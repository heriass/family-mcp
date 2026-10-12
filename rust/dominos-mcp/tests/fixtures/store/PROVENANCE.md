# Frozen Domino’s store states: provenance

Each `<state>.json` here is what the TypeScript Domino’s package left on disk in one state that
`tests/ts/interop.test.ts` had the TypeScript side write and the binary continue. After the
TypeScript package is deleted, these files are how `tests/money.rs` and `tests/persistence.rs`
check that the binary still reads and continues what earlier TypeScript versions wrote. They are
not regenerated.

- Source: `packages/dominos-mcp/src` and `packages/session-store/src` at 41d6f98 (unchanged on
  `feat/ts-retire-1b`); the provider is the parity generator's copy of `tests/ts/fixtures.ts`.
- Bun: 1.4.2, on macOS (arm64), with the store test seam (a file key, never the keychain).
- Generator, from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 bun tooling/ts-freeze/dominos-parity.ts rust/dominos-mcp/tests/fixtures/store store`
- Clock: `Date.now` returned 2100-01-01T00:00:00Z (`now`) while the package ran, so no session,
  quote or checkout has expired when a test reads it. Everything else is as written.
- Data: synthetic. The session tokens are `interop-access`/`interop-refresh` (state `signed-in`) or
  `synthetic-access`/`synthetic-refresh`; the journal records hold the provider's synthetic
  `private-*` Adyen values, as the package wrote them. Each key is a random 32-byte key made for
  its scratch home; it protects only those synthetic tokens. Nothing reached Domino’s, Adyen or
  the real store; the package's `request` was the provider, in-process.

| id `dominos/store/…`  | case                 | the TypeScript calls                                                  |
| --------------------- | -------------------- | --------------------------------------------------------------------- |
| `signed-in`           | `interop.test.ts:23` | `login('5550123', '123456', path, request)` with the case's `request` |
| `quoted`              | `interop.test.ts:45` | `saveSession(saved())`, `migrate`, then `quoteOrder(cart)`            |
| `checkout-ready`      | `interop.test.ts:45` | as `quoted`, then `createCheckout` (`ready`)                          |
| `checkout-authorised` | `interop.test.ts:45` | as `checkout-ready`, then `paySavedCard` (`authorised`)               |

Each file records:

- `now`: the frozen clock, in milliseconds.
- `result`: what the TypeScript calls returned (the public quote, checkout and payment).
- `provider`: the provider's `orders` and `payments` counters afterwards.
- `files`: every file under the home, sorted. Each has its path, its permission bits (octal), and
  its bytes: `text` for `.json` files, `hex` otherwise.
