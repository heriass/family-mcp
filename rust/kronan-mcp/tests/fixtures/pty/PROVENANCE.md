# Frozen Krónan prompt outcomes: provenance

`<platform>.json` holds what the TypeScript CLI's hidden `auth set` prompt did for each case of
`tests/prompt.rs` on that platform's pseudo-terminal. Each case records the exit code or signal,
standard output and error, the tokens sent to the fake `/me/`, and whether the terminal mode was
restored. `prompt.rs::the_hidden_prompt_reads_keystrokes_like_the_frozen_typescript_cli` drives
this binary through the same cases and compares the outcomes, without Bun.

- `macos.json` (`kronan/pty/the-hidden-prompt-reads-keystrokes-like-the-typescript-cli`):
  - source: `packages/kronan-mcp/src` and `tests/ts/rewrite.ts` at 41d6f98, unchanged on
    `feat/ts-retire-1b`;
  - Bun 1.4.2 on macOS;
  - from `rust/`:
    `FAMILY_MCP_BUN="$(command -v bun)" KRONAN_PTY_FREEZE="$PWD/kronan-mcp/tests/fixtures/pty/macos.json" cargo test --locked --all-features -p kronan-mcp --test prompt freeze_the_typescript_prompt_outcomes -- --ignored`.
- `linux.json` (same id):
  - source and generator as `macos.json`, with `KRONAN_PTY_FREEZE=/out/linux.json`;
  - Bun 1.4.2 for Linux (arm64, copied from the `oven/bun:1.4.2` image) in a
    `rust:1.99.0-bookworm` (arm64) container, the worktree mounted read-only;
  - every case equals `macos.json` except the hang-up, where Linux leaves no terminal mode:
    the hang-up case is `Cancelled: the terminal input closed.` with no terminal mode left. Bun met only this outcome in the freeze and five live Linux runs, and the binary's reader (`src/terminal.rs`) ends the same way on end of input and on a failed read, so Krónan has no second hang-up outcome to freeze.

The token is synthetic. The prompt ran with the store test seam in a scratch home; nothing
reached Krónan or the real store.
