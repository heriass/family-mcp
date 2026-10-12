# Changelog

## 0.9.2

Released 2026-10-12.

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

## 0.9.1

- An InfoMentor page with very many attributes in one HTML tag no longer slows
  every call. Every school read and login parses InfoMentor's pages, and the
  check for repeated attributes in a tag took time that grew with the square of
  their number; it now grows linearly.

## 0.9.0

- The native binary is now built from Rust. It serves the same tools, inputs,
  results, CLI commands, messages and exit codes as the TypeScript sources.
- The session and the sign-in a login used are kept in one encrypted record in
  the file store, with its key in a separate private file: on macOS in
  `~/Library/Application Support/family-mcp`, which is excluded from Time
  Machine; on Linux in `~/.config/infomentor-mcp`, with the key in
  `~/.local/share/family-mcp/keys`. Automatic renewal tries that stored sign-in
  first, so the credentials file can be deleted after login; logout removes
  both. `infomentor-mcp status` shows how the session is saved.
- Every start checks the store's location and permissions. A refusal names the
  path and, for most problems, the command that fixes it.
- A session saved by 0.8.0 or earlier keeps working from its plaintext file.
  Stop running servers of the older version, then run
  `infomentor-mcp auth migrate` once (add `--credentials FILE` to store the
  sign-in too) to move it into the encrypted store and remove the file. `login`
  and `login --import` move it as well. Collection snapshots stay in
  `<session file>.collections`. Commands also accept an `auth` prefix.
- Sessions saved with earlier test builds that kept the store key in the macOS
  Keychain are not migrated: remove the files the refusal names, then run
  `infomentor-mcp login` again.
- The installer options `--with-direct-route`, `--without-direct-route`,
  `--with-warp` and `--without-warp` work as before.

## 0.8.0

- Added Linux installer option `--with-direct-route` for hosts where the normal
  InfoMentor destination fails. It verifies HTTPS to both school hostnames via
  InfoMentor's alternate frontend before saving a managed `/etc/hosts` entry.
  Requires Python 3 and administrator access; no VPN or home relay is required.
- Upgrades remember the route and revalidate its current address.
  `--without-direct-route` removes only the managed entry. Existing unmanaged
  overrides and failed preflight checks leave the hosts file unchanged.
- Documented the tested Grok Bot route, WARP limitations, and the separate
  one-time login needed to renew an expired session without a verified account ID.

## 0.7.0

Breaking changes:

- Removed the optional local HTTP password form: `--local-form` and the MCP
  `localForm` field are rejected, and setup status no longer includes `waiting`.
  A stale form could send credentials to another local process after cancellation
  or timeout.
- Use a private credentials file or privately injected `INFOMENTOR_USERNAME` /
  `INFOMENTOR_PASSWORD` instead. Restart upgraded MCP processes and close any old
  local-form browser tabs. Existing sessions, account binding, session import,
  cancellation, and automatic renewal remain supported.

## 0.6.1

- Upgrades now report old running server processes and can stop them with
  `--stop-running`; otherwise restart the MCP host to use the new binary.

## 0.6.0

First release from the `family-mcp` monorepo. Install URLs moved to
`https://github.com/olafurns7/family-mcp/releases/download/infomentor-mcp@0.6.0/...`;
releases under the old repository are not updated.

Breaking changes:

- `infomentor_login`, `infomentor_setup_status`, `infomentor_cancel_setup`, and
  `infomentor_logout` are registered only when `serve` runs with `--allow-setup-tools`.
  By default the server exposes seven read tools and a missing session points to the CLI.
- An explicit `login` or `import` refuses to replace a session verified for a
  different account, or an unreadable session file, unless `--allow-account-change`
  (or `allowAccountChange`) is given.
- `infomentor_setup_status` no longer returns `loginUrl`; the local form URL is
  printed on the server's standard error only.
- New installs store the session at `~/.config/infomentor-mcp/session.json`
  (XDG). An existing `~/.infomentor-mcp/session.json` keeps working.
- Migrated to `@modelcontextprotocol/server` v2. The npm package and library
  entry points are gone; the native executable is the only distribution.

Other changes:

- Feed items (timetable, messages, notifications) are parsed independently: a
  malformed item is skipped and counted in `skipped` and `skippedByFeed`, display
  names may be `null`, and unknown notification `state` values pass through.
  Collection keeps the previous baseline for any feed with skipped rows, so a
  transient upstream glitch cannot produce false "missing" reports.
- A `429 Retry-After` pause is capped at one hour, saved with the session, and
  honoured by every process using that session file.
- The login `--timeout` now covers waiting for the session lock.
- Session, import, and credentials files are read through the shared
  `@family-mcp/session-store` with owner, mode, symlink, hard-link, and size checks;
  writes are `fsync`ed; the lock waits up to 30 seconds and never expires a live process.
- Logout also removes collection snapshots.
- Tool errors are fixed, reviewed messages; upstream text never reaches tool output.
- Tests run under `bun test` with injected seams; a loopback HTTP fixture covers
  redirects, cookies, rate limits, and oversized bodies.

## 0.5.0

Last release from the standalone `infomentor-mcp` repository.
