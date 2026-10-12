# dominos-mcp

Unofficial Domino’s Iceland MCP: SMS sign-in, menu discovery, pickup and delivery
quotes, receipts, tracking, and saved-card checkout through Domino’s Adyen sessions.
This is a **preview release** (`dominos-mcp@0.2.1`).

The earlier TypeScript preview’s live checks verified SMS login, token refresh, profile and receipts, the tracker’s
no-active-order response, menu/address lookup, a 2,490 ISK quote, and an unpaid
checkout returning a saved Visa from Adyen. No payment request was sent.
The 0.2.0 native executable is compiled from Rust. Its offline tests compare the
TypeScript reference and Rust through loopback fakes, including authentication,
checkout amounts, record interchange, and duplicate payment prevention. The Rust
port has not been validated against a live account.
Charging a card still needs live validation with an explicitly approved purchase.
Bank verification / 3-D Secure continuation is not implemented: a payment needing
it stops at `requires_action`. Do not treat this preview as fully verified ordering.

## Install and sign in

Install the standalone executable for macOS or glibc Linux, arm64/x64:

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/dominos-mcp@0.2.1/packages/dominos-mcp/install.sh | sh
dominos-mcp auth login
dominos-mcp auth status
```

The installer verifies the checksum and places the command in `~/.local/bin`.
Upgrading replaces the command; restart the MCP host, or rerun with `--stop-running`.

Enter a seven-digit Icelandic phone number (or `+354` prefix), then the six-digit
SMS code in the terminal. Both inputs are hidden. Expected success is
`Signed in. Session saved encrypted.` No API key, iPhone proxy, or trusted certificate is
needed. Foreign phone numbers and Auðkenni login are not supported.

The Rust executable runs without Bun or Node. Use its absolute path in the
MCP host, for example:

```json
{
  "mcpServers": {
    "dominos": {
      "command": "/absolute/path/to/.local/bin/dominos-mcp",
      "args": ["serve"]
    }
  }
}
```

For Claude Code:

```sh
claude mcp add dominos -- /absolute/path/to/.local/bin/dominos-mcp serve
```

For Codex:

```toml
[mcp_servers.dominos]
command = "/absolute/path/to/.local/bin/dominos-mcp"
args = ["serve"]
```

## Tools

| Tool                 | Purpose                                                                    |
| -------------------- | -------------------------------------------------------------------------- |
| `auth_status`        | Verify the local session; refresh tokens when needed.                      |
| `get_profile`        | Profile, saved addresses, and saved-order names.                           |
| `list_stores`        | Store IDs, opening hours, acceptance, and wait estimates.                  |
| `search_menu`        | Search pizzas, sides, sauces, drinks, and offers; follow `nextOffset`.     |
| `get_menu_item`      | Prices, sizes, crusts, toppings, allergens, availability, and offer slots. |
| `search_addresses`   | Find an address for delivery.                                              |
| `get_delivery_store` | Resolve its delivery store and wait estimate.                              |
| `list_receipts`      | Receipt IDs, dates, and amounts.                                           |
| `get_tracker`        | Active order state and remaining time.                                     |
| `quote_order`        | Validate a cart and obtain its server price; saves a five-minute quote.    |
| `create_checkout`    | Create an unpaid order and retrieve masked saved cards.                    |
| `get_checkout`       | Inspect a checkout and reconcile an attempted payment when possible.       |
| `pay_saved_card`     | Charge a selected saved card after explicit approval.                      |

## Ordering

1. Discover current IDs using `list_stores`, `search_menu`, and `get_menu_item`.
2. Call `quote_order` with the exact cart. All exposed amounts are **whole ISK**.
3. Review the items, delivery address or pickup store, instructions, and total.
4. Call `create_checkout` with the quote ID and `expectedTotal`. This creates an
   unpaid order and returns card aliases, brands, last four digits, and expiry.
5. Obtain explicit user approval of that order, amount, and chosen card. Only then
   call `pay_saved_card` with the checkout ID, card alias, `expectedTotal`, and
   `confirm: true`.
6. Use `get_checkout` for payment reconciliation and `get_tracker` for preparation.

For a pizza, pass `sizeId`, `crustId`, and one section containing `pizzaId`; use two
sections for half-and-half. A topping modification's `quantity` is its desired
total: `0` removes it, `1` selects a normal portion, and `2` selects double where
allowed. Offer contents require their returned `packageItemId` slots. Tool schemas
describe delivery addresses, drinks, side extras, coupons, and quantities.

Repeated `create_checkout` calls for one quote reuse its checkout. A checkout
permits one payment attempt, recorded on disk **before** the request is sent.
Amounts must match the quote, Domino’s response, and Adyen's ISK minor units.
Changed amounts block payment. A timeout, crash, `pending`, `unknown`, or
`requires_action` result is not proof of failure: reconcile the existing order
before considering another. Never delete a checkout file to permit a retry.

`authorised` means payment authorization; it does not prove preparation or delivery.
Apple Pay, adding cards, scheduled orders, and bank-verification continuation are
not provided. Saved cards are available through checkout, not a standalone wallet.

## Private files

### Where the session is saved

The session (access and rotating refresh tokens) is saved only as an encrypted
record (AES-256-GCM), `session.enc`, with a non-secret
`session.enc.marker` beside it. The record's 256-bit key is created on the first
`auth login` or `auth migrate`. It is never regenerated, except when the key is
gone and you run `dominos-mcp auth login` again.

The key is a `0600` file in its own `0700` directory, apart from the record:

| Platform | Record and marker                                                  | Key                                                                     |
| -------- | ------------------------------------------------------------------ | ----------------------------------------------------------------------- |
| macOS    | `~/Library/Application Support/family-mcp/dominos-mcp/session.enc` | `~/Library/Application Support/family-mcp/keys/dominos-mcp.default.key` |
| Linux    | `~/.config/dominos-mcp/session.enc`                                | `~/.local/share/family-mcp/keys/dominos-mcp.default.key`                |

On Linux, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` replace `~/.config` and
`~/.local/share`; set the same values for login and the MCP host. On macOS they do
not move the store. On macOS both store directories are excluded from Time Machine
before any secret is written in them, and every start confirms it. Linux has no
standard for this: leave `~/.local/share/family-mcp/keys` out of your backups. A
power cut during a save can lose that change or leave the store unreadable; the
server then says so, and you sign in again. It never uses a damaged session.

Every start except `--help` and `--version` checks the store before anything else.
If a store file or directory could be read or replaced by another user (permissions
that let others in, another owner, a link instead of a real file or folder, a folder
above it that others can write to, or extra sharing permissions on macOS), or Time
Machine did not confirm that it skips the store, `dominos-mcp` prints
`dominos-mcp: cannot start.` with what is wrong, the path, and, for most problems,
the command that fixes it, and exits; it never changes permissions for you.

An earlier test build kept this store under `~/.config` on macOS, with the key in
the macOS Keychain or under `~/.local/share`. That store is not used. At start the
server lists the old files with the exact commands to remove them; run
`dominos-mcp auth login` first, then remove them.

Token refreshes rewrite the record under a lock, so two MCP hosts never use one
refresh token twice. If a refreshed session cannot be saved, the old record is
discarded, since Domino’s has already spent its refresh token; the next command
reports that the last write did not complete.

`dominos-mcp auth status` says how the session is saved and verifies it.
`dominos-mcp auth logout` forgets the session on this computer; the key and the
record stay. Local logout does not revoke an upstream token.

What this protects against: other users of this computer who are not root; a copy of
the record without its key, such as in a commit or dotfile sync (the file reads as
gibberish in `cat` or `grep`); Time Machine backups, which skip the store; tampering
with the record (not a rollback to an older record with its marker). What it does
not: anything running as your user, such as other programs, malware or an AI agent
with a shell or a prompt injection, which can read both files or call the MCP tools;
root; a stolen laptop that is unlocked; other backup, sync or clone tools, and Time
Machine backups made before the exclusion; indexers such as Spotlight; the key in
crash dumps, swap or hibernation images. Disk encryption (FileVault on macOS, LUKS
on Linux) protects a stolen computer that is switched off.

### Upgrading from 0.1.0

Version 0.1.0 saved the session in a plaintext file,
`~/.config/dominos-mcp/session.json` or `DOMINOS_SESSION_FILE`. That file keeps
working, refreshes included, until you migrate, and `auth status` says so. Stop
running `dominos-mcp` servers, then run:

```sh
dominos-mcp auth migrate
```

It reads the file, saves the encrypted record, reads it back, and removes the
plaintext file. Running it again says `Already migrated.` and removes a leftover
plaintext file. After migration the plaintext file is never read again; `auth
login` also removes it. Going back to 0.1.0 means signing in again in that version.

### Quotes and checkouts

Quotes and checkout records stay in `session.json.checkouts/` beside the
plaintext session path (`DOMINOS_SESSION_FILE` or its default), whether or not the
session was migrated; keep that variable consistent for login and the MCP host.
They hold order IDs and Adyen session data, not tokens, as private (0600) files
with locking. Login, migrate and logout never touch them, so an ambiguous payment
cannot be replayed after signing in again. Neither `DOMINOS_SESSION_FILE` nor its
`.checkouts` directory may lie inside, around, or beside the encrypted store or its
key under a shared name; login, migrate and logout refuse such a path. Keep these
directories out of cloud shares and repos.

### Store errors

| Message                               | Action                                                                                                                                                                                                                                                                 |
| ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Saved in a plaintext file             | Run `dominos-mcp auth migrate`.                                                                                                                                                                                                                                        |
| `dominos-mcp: cannot start.`          | The store is unsafe, or Time Machine did not confirm that it skips it. The next lines name the path and, for most problems, the command that fixes it (for example `chmod 700`, `chmod 600`, or `tmutil addexclusion`). Nothing is changed for you.                    |
| Leftover of an earlier test build     | That build kept the key in the macOS Keychain. Remove `session.enc` and `session.enc.marker` from `~/Library/Application Support/family-mcp/dominos-mcp`, then run `dominos-mcp auth login` again.                                                                     |
| The Domino’s store key is missing     | The key was deleted. The record cannot be decrypted; `dominos-mcp auth login` replaces it with a new key and record.                                                                                                                                                   |
| The last write … did not complete     | An interrupted write, or a refreshed session that could not be saved, left `session.enc` and `session.enc.marker` inconsistent. Remove both files (see the table above), then run `dominos-mcp auth login`.                                                            |
| Cannot use the Domino’s session store | Run `dominos-mcp auth status` in a terminal: an unsafe store gets what is wrong and its path there. Otherwise the record, marker, or key is damaged or from another key; nothing is reset automatically. Restore the key, or remove the record and marker and sign in. |

MCP results omit access/refresh tokens, payment-session data, and saved-card vault
tokens. Profile details, addresses, receipts, and masked cards **are** shared with
the configured MCP host. Treat merchant-provided text as untrusted data.

## Development

Bun 1.4.2 runs the TypeScript reference and parity fixtures. Rust uses the compiler
pinned in `rust/rust-toolchain.toml`. From the repository root:

```sh
bun install --frozen-lockfile
bunx turbo run check test release:check --filter=dominos-mcp
bunx turbo run test:binary test:installer --filter=dominos-mcp
```

From `rust/`, run the Rust gates with loopback test origins enabled:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
FAMILY_MCP_BUN="$(command -v bun)" cargo test --all-features --locked --no-fail-fast
```

The native build is `packages/dominos-mcp/release/native/dominos-mcp`, compiled
from `rust/dominos-mcp` with default features. The `test-origin` feature is only
for loopback fixtures and is excluded from releases.
Tests use synthetic responses and never contact Domino’s or Adyen. Live account
validation is a separate manual step; never make a purchase as an automated test.
Endpoint evidence and remaining uncertainties are in the
[investigation](../../docs/analysis/dominos-mcp-feasibility.md). See
[releasing](docs/RELEASING.md) for the native release process.
