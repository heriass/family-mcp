# Frozen Domino’s contract cases: provenance

Each `<case>.json` here is what the TypeScript Domino’s CLI and server did in one drop-in case the
ledger names `dominos/contract/*`. `tests/replay.rs::every_frozen_contract_case_replays` replays
every file through the binary. The file format, the fake, the normalization and the secret checks
are those of `../parity/PROVENANCE.md`; this file names only what differs.

- Source: as `../parity/PROVENANCE.md`, plus `tests/ts/{reads,catalog,auth,auth-integration}.test.ts`
  at 41d6f98 (unchanged on `feat/ts-retire-1b`).
- Bun: 1.4.2, on macOS (arm64).
- Generator, from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 bun tooling/ts-freeze/dominos-parity.ts rust/dominos-mcp/tests/fixtures/contract contract`
  A second run writes byte-identical files.
- Data: synthetic, as in the parity set.

## Cases

Each drop-in case becomes the CLI runs and tool calls its binary-backed functions made
(`tests/ts/rust-dominos.ts`), and its scripted fetch becomes a fake mode (`contractFetch` in the
generator). A `cli` step may carry its own `env` (`<home>` is the home).

- `no-session-commands` (auth-integration.test.ts:181): `auth status`, `auth migrate` and
  `auth logout` with `DOMINOS_SESSION_FILE` and no session: the first two refuse with the
  sign-in message, logout succeeds, and no record is left (the home's paths are compared).
- `auth-errors-hide-credentials` (auth.test.ts:70): `auth login` whose token request is answered
  400 with a secret body (the drop-in `login` answered its code request), then `auth login` whose
  code request fails in transport (the drop-in `requestCode`, `DOMINOS_SESSION_FILE` under
  `config/dominos-code.json`). No output holds the secrets.
- `offer-slots` (catalog.test.ts:29): five homes, one per `orderItems`, each a saved session and
  one `quote_order` against the two-pizza offer: the valid input (its order body is compared as
  sent), then the quantity, crust, size and pizza changes the case made, each refused before the
  order.
- `public-menu-and-mcp-reads` (reads.test.ts:10): one `serve` lists the tools (compared as values
  with the frozen list, as in the parity surface) and calls `get_profile`, `search_menu` and a
  `quote_order` with an unknown key; then a `serve` whose website answers
  `ReactDOM.hydrate({menu: malicious()})`, the text the case's `parseMenu` refused, through
  `search_menu`. The case's direct `parseMenu(html)` check is the same parse the `search_menu`
  answers exercise.
