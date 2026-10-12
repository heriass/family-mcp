# Frozen Inna parity cases: provenance

Each `<scenario>.json` here records what the TypeScript Inna CLI and server did in one scenario of
`tests/ts/parity.ts`, against that script's fake Inna (`tests/ts/fake-inna.ts`).
`tests/replay.rs::every_frozen_parity_scenario_replays` replays every file through the binary. Once
the TypeScript package is deleted, these files are the contract. Nobody regenerates them.

- Source: `packages/inna-mcp/src`, `packages/session-store/src`, `packages/mcp-runtime/src` and
  `tests/ts/{parity,fake-inna,rewrite}.ts` at 41d6f98. They are unchanged on `feat/ts-retire-1c`.
- Bun: 1.4.2, on macOS (arm64).
- Generator, run from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 bun tooling/ts-freeze/inna-parity.ts rust/inna-mcp/tests/fixtures/parity`
  A second run writes byte-identical files, with one exception: the `plant` lines of the three
  `swap` scenarios. Those hold freshly encrypted records, with a new key, nonce and operation IDs on
  every run.
- Case ids: `inna/parity/<scenario>`, the 38 parity.ts scenario names (`surface`, `reads`,
  `absence-writes`, ...). `replay.rs` lists them all, and the ledger names them.
- Data: all synthetic. The cookies are `synthetic-session`/`synthetic-xsrf` and their rotations
  (`synthetic-rotated-N`, `synthetic-switched`). Every upstream answer comes from fake-inna.ts and
  the scenario's planted replies. Each home was a scratch directory with the store test seam.
  Nothing reached Inna or the real store.

## How the TypeScript side ran

parity.ts ran each scenario twice, against the TypeScript CLI and server and against the binary,
each in its own home, with one fake and one clock file (`INNA_TEST_NOW`, starting at
2040-01-02T12:00Z). It then compared the two runs. The freeze ran only the TypeScript side. It used
the same environment (scratch `HOME`, `TMPDIR` and XDG folders, `INNA_TEST_ORIGIN`, the clock) and
the CLI with `tests/ts/rewrite.ts` preloaded. Between the CLI and the fake sat a recording proxy.
MCP traffic was raw newline-delimited JSON-RPC (protocol 2025-11-25, client `inna-parity`), with one
`serve` per step, as parity.ts ran them. Each answer is the line the server wrote.

Two steps of parity.ts ran differently here:

- A `swap` step ran the binary in the TypeScript home, so that each side read the other's records
  and absence previews. The freeze ran the TypeScript server there instead. It also recorded the
  home as it stood before that step: every path, its mode and its bytes. A replay plants that home,
  so the binary continues from what TypeScript wrote, encrypted record and preview included.
- parity.ts's `LEGACY` plaintext session holds a cookie jar with wall-clock creation times. The
  freeze computed it with the system time set to the scenario clock, so the frozen file is the same
  on every run.

## What each file records

- `id`, `env` (the environment the scenario set besides the scratch folders, `<home>` standing for
  the home), and `rotation` (true where the scenario plants a 401; see the names below).
- `steps`, one per line, in order:
  - `file`: a file parity.ts wrote (folders 0700; the file 0600, or `permissions`). A parity.ts
    `seed` is this cookie file followed by a `cli` step for `auth import <home>/cookies.json`.
    A `legacy` is the `LEGACY` text at its path.
  - `upstream`: the fake state the scenario planted. A replay ignores it, because the recorded
    exchanges carry the answers.
  - `clock`: the new scenario time, in milliseconds.
  - `directory`: a folder's new mode.
  - `cli`: arguments, standard input when given, and the text that must never be printed
    (`absent`). It also records exit code, stdout, stderr, and every upstream exchange of the run.
  - `serve`: arguments. For the surface scenarios it also records the `initialize` and `tools/list`
    answer lines. Each call is a `[tool, arguments]` pair; `<operation>` stands for the last
    prepared operation. For each call it records the notifications before the answer, the answer
    line, and the call's exchanges, then the server's exit code and stderr once its input closed.
    A `swap` step also records `plant` (`{path, mode, hex?}`, a folder without `hex`) and
    `operations`: the operation IDs known before the step, in prepare order.
  - `cancel`: the call, by whom (`host`, `stdin`, `SIGINT`, `SIGTERM`), the path whose request
    Inna was holding (`when`), the fake's delays, how many absence writes reached Inna (`writes`),
    the exchanges, and the `outcome`. For the host's `notifications/cancelled`, the outcome says
    whether the call was still answered before a following `ping`, plus the exit and stderr. For
    the other three it is the exit (`exited`, `signal`). parity.ts compared no output for those.
  - Each `cli`, `serve` and `cancel` step also records `journal`: the text of every absence record
    (`<session>.absence.json`) by path.
- `requests`: the number of upstream requests in the scenario. There are 558 in all.
- `files`: every path left under the home with its mode, sorted. A folder ends in `/`.
- `store`: `"none"` when no record or marker exists; `record`: the decrypted session text (`null`
  once logged out); or `error`: the store's error code.

An exchange is `{request, response, recovery?}`:

- `request`: method, `https://<host><path>?<query>`, the eight headers fake-inna.ts compares
  (`accept`, `authorization`, `content-type`, `cookie`, `origin`, `referer`, `x-requested-by`,
  `x-xsrf-token`; `null` when absent), and the body.
- `response`: status, headers (no `date`, `connection`, `keep-alive`, `content-length` or
  `transfer-encoding`; `set-cookie` as an array), and body, raw.
- `recovery`: present only for a GET answered 401. It holds the renewal (`GET
  https://nam.inna.is/api/UserData/GetLoggedInUser`) and, when the renewal succeeded, the one retry
  of the same URL. Both answers were computed on a copy of the fake's state, without changing it.

## Names

The freeze rewrote these values in requests and outputs. Responses stay raw. The replay rewrites
the binary's outputs and requests the same way before comparing:

- the scratch home → `<home>`;
- the Nth operation ID a prepare returned → `<operation-N>`;
- an ISO time with milliseconds from 2020 up to 2039 → `<wall-clock>`. A cookie jar's creation and
  last access times come from the wall clock, not the scenario clock, which reads 2040 or later.
  No scenario value falls in that span;
- where `rotation` is true, `synthetic-rotated-<digits>` → `synthetic-rotated-<rotation>`. The
  renewal the binary may add after a 401 shifts the fake's count; parity.ts normalized it the same
  way.

Long runs of one character inside a string are packed as U+0001, the character, `*`, the count,
U+0001. The sentinel is U+0001 because login-input's standard input holds a NUL. The replay expands
the runs.

The package version is replaced, not normalized in the fixtures: the freeze recorded inna-mcp
0.4.1, as `--version` and `-v` stdout (`0.4.1\n`) and as the `initialize` answer's
`result.serverInfo.version`. At those two fields only, the replay requires exactly that recorded
value and expects this binary's own version (`CARGO_PKG_VERSION`) in the same format instead;
a different recorded value or format, a missing field, or a wrong binary version fails
(`only_the_recorded_version_becomes_the_binary_version`). The fixture bytes are unchanged.

## What the replay allows, and what it relaxes

- **The recovery hop.** parity.ts let the binary renew once after any GET answered 401 and then
  retry that URL once. The replay allows exactly that hop and answers it from `recovery`. It counts
  the hops: a scenario's request total is the recorded count plus the hops. renewal.rs executes the
  recovery itself.
- **The keep-alive opt-out text.** The binary's help says "Disable optional 10-minute touches;
  mandatory renewal still runs before 30 minutes". parity.ts mapped that to the TypeScript text
  ("Do not touch the saved session every 10 minutes while serving") before comparing, and the
  replay does the same.
- **Key order.** parity.ts read `initialize` and `tools/list` through the SDK client, which rebuilds
  them in its schemas' key order, so only their values were ever compared. The replay compares
  their values too. A tool answer's key order is compared exactly.
- A cancelled call holds Inna's reply to `when` until the cancel, end of input or signal has been
  delivered, then releases it. The replay never sleeps. Each later step's script is strict, so a
  request sent late fails the step it arrives in.
