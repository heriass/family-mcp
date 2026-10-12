# Changelog

## Unreleased

- `serve` now ends when the connection to the host breaks, as the TypeScript
  server does: when a write to stdout fails because the host closed its end,
  or when one input line passes 10 MiB, its newline included. That limit is
  exact; the TypeScript server can also close on a shorter line, depending on
  how the input arrives in reads, which is an intended difference. Calls in
  flight are cancelled at once and the server closes as on SIGTERM, then the
  process exits with code 0 and prints nothing. Before, it kept running until
  stdin ended, and it buffered an over-long line without limit.
- After stdin ends, `serve` now also exits within about 6 s while an answer is
  still waiting on a stdout the host has stopped reading: the usual 5 s wait
  for calls in flight, then calls are cancelled and the server closes. Before,
  it could wait for that write forever. The TypeScript server does not exit in
  that case either.

## 0.7.1

- Pressing Ctrl-C (or sending SIGTERM) while `abler-mcp auth login` is still
  starting the browser now gives the browser one more chance to answer (up to
  about 2 s more). If it answers, abler-mcp closes it through its private
  debugging pipe (`Browser.close`) before falling back to SIGTERM. A browser
  that answers but ignores `Browser.close` is signalled after up to 5 s more.
  The temporary profile is still removed and nothing is saved. Messages and
  exit codes are unchanged.

## 0.7.0

- The native binary is now built from Rust. Tools, inputs, results, CLI
  commands and messages are unchanged.
- The session is kept only in the encrypted file store: on macOS in
  `~/Library/Application Support/family-mcp`, which is excluded from Time
  Machine; on Linux in the XDG data and config folders. The macOS Keychain is
  not used.
- Every start checks the store's location and permissions. A refusal names the
  path and, for most problems, the command that fixes it.
- After upgrading from 0.6.0 or earlier, run `abler-mcp auth migrate` once.
- Sign-ins made with earlier test builds are not migrated: run
  `abler-mcp auth login` again. Their leftover files are named with removal
  commands.

## 0.6.0

- Add `list_conversations` and `list_messages`, two read-only tools for the
  signed-in account's Abler messages. `list_conversations` returns the total
  unread count and a page of conversations, most recently active first, each
  with its unread count and latest message. `list_messages` pages one
  conversation, newest message first.
- The tools do not mark messages as read and cannot send messages. Attachment
  URLs are not returned. Message, conversation, and attachment text is
  untrusted data.

## 0.5.3

- Cookie-file imports now require a private, regular, owner-controlled file;
  symlinks and hard links are rejected. File and stdin imports are limited to
  4 MiB before parsing or account verification.
- Malformed upstream authentication cookies and unreviewed CLI exceptions now
  produce fixed, safe errors. Browser login and capture retain useful reviewed
  diagnostics without exposing upstream or library exception text.

## 0.5.2

- Upgrades now report old running server processes and can stop them with
  `--stop-running`; otherwise restart the MCP host to use the new binary.

## 0.5.1

- Fix `list_schedule`, `list_child_schedules`, and `get_event` rejecting every
  event: Abler sends `arrivalTime` as a number (minutes before the start), and the
  0.5.0 output schema only accepted a string. Numeric and string values are both
  accepted now; the value is passed through unchanged.

## 0.5.0

- Added `auth login` with isolated temporary profiles: browser login uses a
  private pipe transport; no debugging port is opened.
- Close the browser before verification, keep signal cleanup idempotent through
  profile removal, and sweep abandoned login profiles older than one hour.
- Keep `--keep-browser` available for debugging while detaching the CLI; its
  retained browser has no debugging endpoint after the pipe closes.

## 0.4.0

First release from the `family-mcp` monorepo. Install URLs moved to
`https://github.com/olafurns7/family-mcp/releases/download/abler-mcp@0.4.0/...`;
releases under the old repository are not updated.

- All six tools now declare strict output schemas and return `structuredContent`.
  Unknown upstream fields are stripped instead of forwarded; `list_groups` is validated.
- Tool errors are fixed, reviewed messages. Upstream response text, tokens, and
  cookies never reach tool output.
- `auth logout` also removes `.pending` candidates left by failed imports, and a
  later verified import prunes earlier candidates.
- Session storage moved to the shared `@family-mcp/session-store`: owner, mode,
  symlink, and hard-link checks on reads; `fsync` on writes; a dependency-free lock
  that waits up to 30 seconds and recovers dead owners but never expires a live process.
  `proper-lockfile` was removed.
- SIGINT/SIGTERM abort in-flight Abler requests, release the session lock, and exit promptly.
- Abler responses are capped at 4 MiB before parsing.
- Migrated to the shared native-only release tooling; the npm tarball is gone.

## 0.3.1

Last release from the standalone `abler-mcp` repository.
