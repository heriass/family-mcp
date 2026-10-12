# Frozen Domino’s prompt outcomes: provenance

`<platform>.json` holds what the TypeScript CLI's hidden `auth login` prompt did for each case of
`tests/prompt.rs` on that platform's pseudo-terminal. Each case records the exit code or signal,
standard output and error, the requests the fake saw, and whether the terminal mode was restored.
`prompt.rs::the_hidden_prompt_reads_keystrokes_like_the_frozen_typescript_cli` drives this binary
through the same cases and compares the outcomes as the live test does (`compare`), without Bun.

- `macos.json` (`dominos/pty/the-hidden-prompt-reads-keystrokes-like-the-typescript-cli`):
  - source: `packages/dominos-mcp/src` and `tests/ts/rewrite.ts` at 41d6f98, unchanged on
    `feat/ts-retire-1b`;
  - Bun 1.4.2 on macOS (arm64);
  - from `rust/`:
    `FAMILY_MCP_BUN="$(command -v bun)" DOMINOS_PTY_FREEZE="$PWD/dominos-mcp/tests/fixtures/pty/macos.json" cargo test --locked --all-features -p dominos-mcp --test prompt freeze_the_typescript_prompt_outcomes -- --ignored`.
- `linux.json` (same id):
  - source and generator as `macos.json`, with `DOMINOS_PTY_FREEZE=/out/linux.json`;
  - Bun 1.4.2 for Linux (arm64, copied from the `oven/bun:1.4.2` image) in a
    `rust:1.99.0-bookworm` (arm64) container, the worktree mounted read-only;
  - every case equals `macos.json` except the hang-up, where Linux leaves no terminal mode:
    the hang-up case records the outcome Bun met and, as `allowed`, the exact pair of outcomes either side may meet there (EOF-cancel or EIO-generic, PR #21); the binary must give one of the pair.

The Bun reference may ignore some editing keys (taskr decisions 76087/76089/76096/76141); for
those cases the frozen outcome is one of the two that `check_keystrokes` allows, and the binary
must give the documented one. The phone number, SMS code and tokens are synthetic. The prompt
ran with the store test seam in a scratch home; nothing reached Domino’s or the real store.
