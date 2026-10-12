# Frozen Inna contract cases: provenance

Each `<case>.json` here records what the TypeScript Inna CLI and server did in one drop-in case.
The ledger names these cases `inna/contract/*`; they come from `tests/ts/integration.test.ts`,
`login.test.ts` and `browser-login.test.ts`. `tests/replay.rs::every_frozen_contract_case_replays`
replays every file through the binary. Once the TypeScript package is deleted, these files are the
contract. Nobody regenerates them.

- Source: `packages/inna-mcp/src`, `packages/session-store/src`, `packages/mcp-runtime/src` and
  `tests/ts/{integration,login,browser-login}.test.ts`, `rust-inna.ts` and `rust-login.ts` at
  41d6f98. They are unchanged on `feat/ts-retire-1c`.
- Bun: 1.4.2, on macOS (arm64).
- Generator, run from the repository root:
  `BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 bun tooling/ts-freeze/inna-parity.ts rust/inna-mcp/tests/fixtures/contract contract`
  A second run writes byte-identical files, with one exception: the two `plant` lines of
  `too-many-saved-students`. Those hold freshly encrypted records.
- Data: all synthetic. Inna is the case's own scripted upstream, copied unchanged from the test
  files: integration.test.ts's `Provider`, login.test.ts's `provider()` and rust-login.ts's
  `user()`. A case may also wrap it, as the test did. Nothing reached Inna, island.is or the real
  store.

## The cases

| File | From | What it freezes |
| --- | --- | --- |
| `read-tools-round-trip` | integration.test.ts:460 | the read-only surface and all 14 read tools; only GETs, no mark-read or external fetch |
| `message-continuation` | :590 | three delivered pages, then an empty, a duplicate and a miscounted page refused |
| `illness-history-blocks-requests` | :781 | a malformed and an overlapping illness history each refuse a sick and a leave request; no POST |
| `dated-feeds` | :806 | every dated feed: parsed, missing and unrecognized dates beside their source text |
| `default-user-id` | :1177 | the stored default account survives a sibling read and is gone after logout |
| `students-listed-without-switching` | :1242 | the student list, made without a switch; a keyed read then switches once |
| `single-default-context` | :1289 | a session with no student entries lists its context as the single default; a duplicate list is refused |
| `unknown-student-keys` | :1417 | unknown, non-student and malformed keys are refused without a switch |
| `sibling-tool-round-trips` | :1642 | all 16 tools for the sibling key, its keyed submit refused, the plain submit sent once |
| `keep-alive-opt-out-flag` | :1916 | `--no-keep-alive` refused outside `serve` |
| `too-many-saved-students` | :2396 | a 65th student refused with only the jar written back; a stored record or plaintext file holding 65 refused before any request |
| `phone-login-refusals` | login.test.ts:214 | an external redirect, unaccepted terms and an access list without a student context, each refused |
| `phone-login-student-context` | login.test.ts:242 | six sign-ins: the first student context, or the preferred one by its original index |
| `browser-options-only-with-google-login` | browser-login.test.ts:729 | `--browser` and `--timeout` only with `auth login --google`; no browser started |
| `strict-inputs-of-every-other-tool` | ledger :1642 | an invalid input for `inna_get_attendance` and `inna_get_materials`, the two tools no parity scenario gives one, and an unknown key for each of the 16 |

Together with the parity scenarios, every one of the 16 tools has a frozen input-validation error.

## How the TypeScript side ran

The drop-ins replaced the TypeScript client with the binary. A client's reads were tool calls on
one `serve` for the client's life; an import, a migration and a logout were `auth` commands; and
a sign-in was `auth login` with the phone number on standard input. The freeze ran the same steps
against the TypeScript CLI and server, in parity.ts's environment (scratch `HOME`, `TMPDIR` and XDG
folders, the clock file at 2040-01-02T12:00Z), with three differences:

- the case's `INNA_SESSION_FILE`, as the drop-in harness set it;
- `tooling/ts-freeze/inna-rewrite.ts` preloaded. It is rewrite.ts, plus routing for
  `innskra.island.is`, which the sign-in reaches;
- a recording proxy in front of the case's upstream.

Each case asserted what its test asserted wherever the outcome is visible there, so a frozen case
cannot have drifted from its row. A case with several fixtures in its test runs them one after
another in one home:

- `illness-history-blocks-requests` imports again for its second history;
- each sign-in of the two login cases has its own provider;
- `phone-login-student-context` logs out between sign-ins. Where the test preferred a user id, it
  first imports a session (`synthetic-saved`) whose default student is that id, as rust-login.ts
  saved one;
- the unmigrated half of `too-many-saved-students` removes the record, its marker and its key,
  and writes the plaintext file. The freeze set that file's cookie-jar times to the scenario
  clock, so the file is the same on every run.

The drop-in read the default user id and changed the store through the TypeScript store. Here
those are `stored` steps (the decrypted record and the marker's generation) and `plant` steps
(the home as the TypeScript store left it).

## What each file records

The fields, names and packing are those of `../parity/PROVENANCE.md`, plus these steps:

- `open`: a `serve` that stays open across the steps up to its `close`, with its arguments. In
  the two surface cases it also records the `initialize` and `tools/list` answers.
- `call`: one tool call on the open `serve`, with the notifications before its answer, the
  answer line, its exchanges and `journal`.
- `close`: the end of the open `serve`'s input, its exit code and stderr.
- `remove`: a file removed.
- `plant` (with `operations`): the home as the TypeScript store wrote it.
- `stored`: what the store held at that point; `generation`: the marker's generation.

An exchange whose upstream dropped the connection has status 0. The replay drops it the same way.
No case here sends a GET that is answered 401, so no exchange carries `recovery`. The freeze fails
if one does.

## What the replay allows

Every request must equal the recorded one, in order: method, URL with its query, the eight
compared headers, and body. That includes each sign-in's island.is and inna.is requests and its
bearer token. The binary's poll waits are real (2 and 3 seconds per sign-in), as the TypeScript
side's were. `initialize` and `tools/list` are compared by value only; tool answers by value and
key order.
