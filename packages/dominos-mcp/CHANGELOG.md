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

## 0.2.0

- Builds the native executable from Rust with rustls and locked dependencies.
  Installed hosts need neither Bun nor Node.
- Retains the 13 tools, their descriptions and schemas, and the quote/confirm
  flow. Synthetic tests compare CLI, tool results and upstream requests with
  the retained TypeScript reference.
- Implements hidden SMS sign-in and token rotation through the encrypted
  family-store, with session and quote/checkout records readable by both ports.
  Existing plaintext sessions keep their legacy refresh path until
  `dominos-mcp auth migrate`; new sign-ins save encrypted sessions.
- Each newly saved session is kept in one encrypted record, with its key in a
  separate private file. On macOS the record is
  `~/Library/Application Support/family-mcp/dominos-mcp/session.enc` and the key
  is `~/Library/Application Support/family-mcp/keys/dominos-mcp.default.key`;
  these store folders are excluded from Time Machine. On Linux the record is
  `~/.config/dominos-mcp/session.enc` and the key is
  `~/.local/share/family-mcp/keys/dominos-mcp.default.key`, using
  `$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` when set. `dominos-mcp auth status`
  shows how the session is saved.
- Every start checks the store's location and permissions; help and version
  do not touch the store. A refusal names the path and, for most problems,
  the command that fixes it.
- Sessions saved with earlier test builds that kept the store key in the macOS
  Keychain are not migrated: remove the files the refusal names, then run
  dominos-mcp auth login again.
- When `auth login` is ended by SIGINT or SIGTERM while it waits at a hidden
  phone or SMS-code prompt, it restores the terminal and exits with status
  130 or 143. In the TypeScript CLI, the process was ended by the signal.
- Saves checkout and payment intent before sending. Offline tests cover
  concurrent confirmations, mismatched amounts, lost responses, failed
  persistence, process termination and restart without replaying a payment.
- The Rust port has not been checked against a live account or used to charge
  a card. Bank verification / 3-D Secure continuation still stops at
  `requires_action`.

## 0.1.0

- First preview release of the unofficial Domino’s Iceland MCP server.
- Adds terminal SMS login, private session storage, and token refresh.
- Exposes 13 tools for menu and store discovery, pickup and delivery quotes,
  profiles, receipts, tracking, unpaid checkout, and confirmed saved-card payment.
- Checks payment amounts against the quote and payment session, and records one
  payment attempt per checkout to prevent duplicate submission across restarts.
- Distributes standalone macOS and glibc Linux executables for arm64 and x64,
  with SHA-256 checksums and an installer.
- Live checks verified login, refresh, account reads, menu and address lookup,
  quotes, and saved-card retrieval from an unpaid Adyen checkout.
- Card charging remains untested; no purchase was made. Bank verification / 3-D
  Secure continuation is not implemented and stops at `requires_action`.
