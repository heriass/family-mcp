# Frozen Inna prompt outcomes: provenance

`<platform>.json` holds what the TypeScript CLI's hidden `auth login` phone prompt did on that
platform's pseudo-terminal, for both PTY tests of `tests/prompt.rs`:

- `cases` (`inna/pty/the-hidden-phone-prompt-reads-keystrokes-like-the-typescript-cli`): each
  case of `cases()` with its exit code or signal, standard output and error, the requests that
  reached the fake Inna, and whether the terminal mode was restored. On Linux the hang-up case
  also freezes `allowed`: the two outcomes a hang-up can have (a failed read, the generic failure;
  or the input's end, cancelled), one of which Bun met.
- `order` (`inna/pty/a-key-typed-as-the-prompt-shows-is-read-raw-and-not-echoed`): a number typed
  the moment the prompt shows, with standard error held full until then. The terminal was raw
  before the prompt was written and nothing was echoed; those are asserted on every run.

`prompt.rs::the_hidden_phone_prompt_reads_keystrokes_like_the_frozen_typescript_cli` and
`prompt.rs::a_key_typed_as_the_prompt_shows_is_read_raw_like_the_frozen_typescript_cli` drive this
binary through the same cases and compare its outcomes with these, without Bun. The documented
differences apply as in the live cases: the binary restores the terminal after a second SIGINT,
exits 128 + n where Bun dies of signal n, and the two editing cases allow only the documented
result (taskr 76066/76588/76640/76644).

- `macos.json`:
  - source: `packages/inna-mcp/src` and `tests/ts/rewrite.ts` at 3def461, unchanged on
    `feat/ts-retire-1c`;
  - Bun 1.4.2 on macOS;
  - from `rust/`:
    `FAMILY_MCP_BUN="$(command -v bun)" INNA_PTY_FREEZE="$PWD/inna-mcp/tests/fixtures/pty/macos.json" cargo test --locked --all-features -p inna-mcp --test prompt freeze_the_typescript_prompt_outcomes -- --ignored`.

- `linux.json`:
  - source as for `macos.json`;
  - Bun 1.4.2 (linux-arm64, copied from `oven/bun:1.4.2`) in `rust:1.99.0-bookworm`, run as
    uid 65534 with the worktree mounted read-only;
  - the same command with `FAMILY_MCP_BUN=/opt/bun/bun` and `INNA_PTY_FREEZE=/out/inna-linux.json`,
    then copied here.
  - It equals `macos.json` except for the hang-up. Bun met the failed read (the generic failure,
    nothing sent, no terminal mode left), and the case also freezes the pair as `allowed`.

The phone number is synthetic. The prompt ran with the store test seam in a scratch home, and the
fake Inna refused the sign-in's first request; nothing reached Inna or the real store.
