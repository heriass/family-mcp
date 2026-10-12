# Frozen Domino’s parity cases: provenance

Each `<case>.json` here is what the TypeScript Domino’s CLI and server did in one case of
`tests/ts/parity.ts`, `reads-parity.ts`, `auth-parity.ts` and `money-parity.ts`, against those
scripts' own fake answers. `tests/replay.rs` replays every file through the binary. These files are
the contract once the TypeScript package is deleted; they are not regenerated.

- Source: `packages/dominos-mcp/src`, `packages/session-store/src`, `packages/mcp-runtime/src` and
  `tests/ts/{parity,reads-parity,auth-parity,money-parity,fixtures,cli,rewrite}.ts` at 41d6f98
  (unchanged on `feat/ts-retire-1b`).
- Bun: 1.4.2, on macOS (arm64).
- Generator, from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 bun tooling/ts-freeze/dominos-parity.ts rust/dominos-mcp/tests/fixtures/parity`
  A second run writes byte-identical files.
- Case ids (`dominos/parity/<name>`), as the ledger names them:
  - `cli-argument-list` (parity.ts, the 22 argument lists, one home);
  - `tool-surface-and-instructions` (the `initialize` and `tools/list` answers);
  - `invalid-tool-input-list` (48 calls in one `serve`, each an error);
  - `reads-parity` (reads-parity.ts: the 11 reads and 10 failure modes in one home, and
    `refresh-not-modified` in a second home with an expired session);
  - `auth-parity` (auth-parity.ts: 11 `auth login` runs, each in its own home);
  - `unsafe-config-folder-refusal` (parity.ts's last check);
  - the 54 money-parity.ts scenarios, one file each, named by the scenario with every run of
    other characters than `a-z0-9` lowercased to `-` (`payment-already-requires-action`,
    `authorised`, `order-lost`, ...).
- Data: synthetic. The sessions are `synthetic-access`/`synthetic-refresh` (and the login's
  `parity-access`/`parity-refresh`); every upstream answer comes from `tests/ts/fixtures.ts` and the
  parity scripts' modes. Each home was a scratch directory with the store test seam; nothing
  reached Domino’s, Adyen or the real store.

## How the TypeScript side ran

parity.ts compared the TypeScript CLI and server with the binary for the argument list, the
surface and the invalid inputs; reads-parity.ts and money-parity.ts compared the package's
in-process `DominosClient` with the binary's drop-in client, which made each tool call in its own
`dominos-mcp serve`. The freeze ran the TypeScript side alone through the package's CLI and server
(`tests/ts/cli.ts` for the arguments, `--preload tests/ts/rewrite.ts` to route its fetch to a
loopback fake), one `serve` per tool call as the drop-in client made them, so each recorded value is
what the binary must produce on the same wire. The fake is `tests/ts/rust-dominos.ts`'s `upstream`:
`/api/` is Domino’s API, `/website/` its website, `/adyen/` Adyen, and an answer the scripts' fetch
threw becomes a closed connection.

`setup()` saved a plaintext session (`saveSession`) and then migrated it; the freeze migrated with
the package's own `migrate`, and a replay writes the same file (expiring an hour from its own now, or
a millisecond ago) and runs the binary's `auth migrate`, as the drop-in's `setup` did.

## What each file records

- `homes`: one entry per scratch home, each with:
  - `env`: the environment the case set besides the scratch folders (`DOMINOS_SESSION_FILE` as
    `<home>/session.json` for reads and money; none for the CLI, surface and login cases);
  - `steps`, in order:
    - `seed`: the saved-then-migrated session (`expired` true only for `refresh-not-modified`);
    - `mode`: which of the scripts' fake answers applied from then on (a replay ignores it: the
      recorded exchanges carry the answers);
    - `file`: a journal record money-parity.ts planted with `writePrivateFile` (folders 0700,
      file 0600);
    - `dir`: a folder made with the mode given (the unsafe folder, 0755);
    - `cli`: arguments, standard input when given, exit code, stdout, stderr, and every upstream
      exchange the run caused;
    - `serve`: per tool call, the call, the answer line the server wrote (raw JSON-RPC), any
      notifications before it, and every upstream exchange the call caused; then the exit code and
      stderr once standard input closed. The surface case also records `initialize` and
      `tools/list`. Initialize sent protocol version `2025-11-25` and client `dominos-parity`
      1.0.0.
    - An exchange is the request the fake saw (method, path and query, the `authorization`,
      `accept`, `content-type` and `content-length` headers, body) and the response it served
      (status, headers, body; status 0 is a closed connection without an answer).
  - `requests`: the number of exchanges in the home;
  - `store`: `{"store": "none"}` when neither record nor marker exists, else the decrypted session
    record text (`{"record": ...}`, `null` once logged out) or `{"error": CODE}`;
  - `files`: every path under the home with its permission bits (a folder ends in `/`), sorted
    after normalization; `session.json` and every file under `session.json.checkouts` (the quote
    and checkout journal) also with its text.

## Normalization

Applied to CLI output, serve answers, notifications and stderr, the stored record, and the file
paths and texts, in that order within a home, by the generator and by the replay alike:

- `<home>`: the scratch home's real path. Each home is its own.
- `<uuid-N>`: a random version-4 id (a new quote or checkout id), numbered by first appearance in
  the home, so a checkout file named by the id an answer returned keeps that link. The scripts' own
  `10000000-0000-4000-8000-…` ids are kept. A random id that first appears in a file name, not in an
  output, stops the generator.
- `<now±Nm>`: an epoch-millisecond integer (13 digits, starting with 1) within a day of now, as N
  whole minutes from now, rounded (session and quote expiries, quote creation). `<iso±Nm>`: the same
  for an ISO time with milliseconds. Fixed times (`createdAt: 0`, the year-2100 expiries) stay.
- Packing: a run of 1024 or more of one character inside a JSON string is written as
  `\u0000<character>*<count>\u0000` (the `large` body of 8 MiB and one byte in
  `reads-parity`, the oversized record of 1 MiB and one byte in `local-record-oversized`). The generator refuses to run if any recorded value holds a NUL, and the replay expands
  the runs before comparing.

Requests are not normalized: the generator stops if a request names a random id, a time near now or
the home. One transport detail is not compared: a request with an empty body carries
`content-length: 0` from Bun's fetch and no length from the binary's client; the replay drops that
header from both sides when the body is empty. Every other header and every body is compared as
recorded.

The package version is replaced, not normalized in the fixtures: the freeze recorded dominos-mcp
0.2.0, as `--version` and `-v` stdout (`0.2.0\n`) and as the `initialize` answer's
`result.serverInfo.version`. At those two fields only, the replay requires exactly that recorded
value and expects this binary's own version (`CARGO_PKG_VERSION`) in the same format instead;
a different recorded value or format, a missing field, or a wrong binary version fails
(`only_the_recorded_version_becomes_the_binary_version`). The fixture bytes are unchanged.

Nothing else is normalized. Tool answers are compared value for value, key order included. The
`initialize` and `tools/list` answers are compared as values: rmcp orders schema keys differently
from zod, parity.ts compared the surfaces as values, and `src/surface.json` pins the contract on
its own.

## Secrets

No output may hold `secret-`, `private-`, `synthetic-secret`, `parity-access`, `DO-NOT-RETURN`, or a
session token: the generator refused to write such an output, and the replay asserts the same on
the binary's outputs. Requests, fake answers and journal files hold the scripts' synthetic secrets
by design.

## What a replay is not

Timing- and restart-dependent behaviour (a lost response, a kill or restart mid-send, concurrent
refresh, lock takeover) is never replayed; `tests/money.rs`, `tests/session.rs` and the other
executing tests own it.
