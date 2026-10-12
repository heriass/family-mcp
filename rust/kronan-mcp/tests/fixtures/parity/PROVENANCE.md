# Frozen Krónan parity scenarios: provenance

Each `<scenario>.json` here is what the TypeScript Krónan CLI and server did in one scenario of
`tests/ts/parity.ts`, against that script's own fake upstream. `tests/replay.rs` replays every
file through the binary. These files are the contract once the TypeScript package is deleted;
they are not regenerated.

- Source: `packages/kronan-mcp/src`, `packages/session-store/src` and `tests/ts/{parity,fixtures,rewrite}.ts`
  at 41d6f98 (unchanged on `feat/ts-retire-1b`).
- Bun: 1.4.2, on macOS.
- Generator, from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 bun tooling/ts-freeze/kronan-parity.ts rust/kronan-mcp/tests/fixtures/parity`
- Case ids (`kronan/parity/<name>`): `surface`, `cli`, `reads`, `writes`, `write-inputs`,
  `write-statuses`, `orders`, `orders-store`, `orders-migrated`, `order-failures`, `inputs`,
  `edge`, `mismatch`, `statuses`, `store`, `tokens`, `auth`, `legacy`, `legacy-invalid`,
  `attempts`, `attempts-file`, `token-file`.
- Data: synthetic. The token is `synthetic-token-0123456789` (and the made-up variants the
  scenarios set); every upstream answer comes from `tests/ts/fixtures.ts` or the fake's modes.
  Each scenario ran in its own scratch home with the store test seam; nothing reached Krónan or
  the real store.

## What each file records

- `env`: extra environment the scenario set (`KRONAN_TOKEN_FILE`).
- `steps`, in order:
  - `file`, `mode`, `save`: the inputs, as parity.ts wrote them. A replay plants the file, ignores
    the mode (the recorded exchanges already carry its answers) and writes the record through
    `family_store`.
  - `cli`: arguments, standard input when given, exit code, stdout, stderr, and every upstream
    exchange the run caused.
  - `serve`: per tool call, the call, the answer line the server wrote (raw JSON-RPC), any
    notifications before it, and every upstream exchange the call caused; then the exit code and
    stderr once standard input closed. `surface` also records the `initialize` and `tools/list`
    answers. Initialize sent protocol version `2025-11-25` and client `kronan-parity` 1.0.0.
  - An exchange is the request the fake saw (method, path and query, the `authorization`,
    `accept`, `content-type` and `content-length` headers, body) and the response it served
    (status, headers, body). Bodies are the exact bytes as UTF-8, a byte order mark included.
- `requests`: the number of exchanges in the scenario.
- `store`: `{"store": "none"}` when neither record nor marker exists, else the decrypted record text
  (`{"record": ...}`, `null` once logged out) or `{"error": CODE}`.
- `files`: `.config/kronan-mcp` sorted; `.json` files with their text, other entries by name.

## Normalization

- `<home>`: the scratch home, as given and as its real path, in CLI output and serve stderr
  (parity.ts `local`). Each run has its own home.
- `<uuid>` and `<now>`: in CLI stdout and in `.json` file texts, a `"id":"<v4 uuid>"` and an ISO
  time within 60 s of now (parity.ts `fresh`). Attempt ids are random and times are the run's.
  The replay applies the same rules to the binary's output; fixed past times stay as written.
- `<origin>`: the fake's own `http://127.0.0.1:<port>` in recorded response headers and bodies
  (the redirect `location` values). The replay's fake substitutes its own origin.
- Packing: a run of 1024 or more of one character inside a JSON string is written as
  `\u0000<character>*<count>\u0000` (the 4 MiB product bodies, the 16 KiB token inputs). The
  generator refuses to run if any recorded value holds a NUL, and the replay expands the runs
  before comparing.

Nothing else is normalized. Tool answers are compared value for value and key order included.
The `tools/list` and `initialize` answers are compared as values: rmcp orders schema keys
differently from zod, parity.ts never compared that order, and `tests/surface.rs` checks the
surface independently.

## What a replay is not

Timing- and restart-dependent behaviour (a lost response, a kill or restart mid-send, renewal,
lock takeover) is never replayed; `tests/money.rs` and the other executing tests own it.
