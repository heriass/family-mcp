# abler-mcp

Unofficial, read-only [Abler](https://www.abler.io) schedules and messages MCP server. Written in TypeScript and compiled with Bun into a single executable, including its runtime and dependencies. The running MCP server uses HTTP and needs no browser.

For agent setup and reporting rules, read **[docs/AGENTS.md](docs/AGENTS.md)**.

## Quick start (0.5.0+)

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/abler-mcp@0.7.1/packages/abler-mcp/install.sh | sh
```

```sh
abler-mcp auth login
```

`auth login` is available from 0.5.0. With 0.4.0, use `auth capture` or
`auth import` below.

Expected output: `Sign in to Abler in the browser window that opened.`

## Install and connect

The installer verifies the archive checksum and executable version, then installs
`abler-mcp` under `~/.local/bin`. Set `ABLER_PREFIX` to an absolute installation
prefix or `ABLER_VERSION` to a released version before running it. Verify an install:

```sh
/absolute/path/to/.local/bin/abler-mcp --version
```

Expected output: the installed version, such as the one after `abler-mcp@` in the Quick
start installer URL.

Run the Quick start installer again to upgrade; the saved session stays in place.
After upgrading from 0.6.0 or earlier, run `abler-mcp auth migrate` once (see
[Upgrading from 0.6.0](#upgrading-from-060)).
Upgrading replaces the command but not a running server; restart the MCP host, or
rerun with `--stop-running`.
To uninstall the command and release directories while retaining the saved session:

```sh
rm -f /absolute/path/to/.local/bin/abler-mcp
rm -rf /absolute/path/to/.local/share/abler-mcp
```

Paths written as `/absolute/path/...` are on the computer running the MCP host;
configuration files do not reliably expand `~` or `$HOME`.

### Connect to your MCP host

**Claude Desktop** — add only this Abler entry to its MCP JSON configuration.

```json
{
  "mcpServers": {
    "abler": {
      "command": "/absolute/path/to/.local/bin/abler-mcp",
      "args": ["serve"]
    }
  }
}
```

**Claude Code** — run this in the project where Claude Code should use Abler.

```sh
claude mcp add abler -- /absolute/path/to/.local/bin/abler-mcp serve
```

**Codex** — add only this Abler entry to `/absolute/path/to/.codex/config.toml`.

```toml
[mcp_servers.abler]
command = "/absolute/path/to/.local/bin/abler-mcp"
args = ["serve"]
```

If you set `XDG_CONFIG_HOME`, `XDG_DATA_HOME` or `ABLER_SESSION_FILE`, set the same
values for the MCP host as for `abler-mcp auth` commands. On macOS the XDG variables
do not move the encrypted store.

## Authenticate once, then run headlessly

Initial sign-in happens in Abler's own browser UI using phone/email and a code, or Google. After sign-in, the package renews the access cookie automatically and saves cookie rotation. The refresh credential is an Abler session; no Google credentials are collected.

### Sign in with a temporary browser

Run the primary sign-in command:

```sh
abler-mcp auth login
```

It opens a fresh browser profile and waits up to 15 seconds for the browser to
start. `--timeout <seconds>` sets the cookie wait (300 seconds by default). It
captures only the `refreshToken` and `id_token` cookies; login confirms the
browser is closed before session verification and saving. If closure cannot be
confirmed, login exits with an error and still removes the temporary profile.
Verification adds network time after the cookie wait. At startup, login removes abandoned
`abler-login-*` profiles older than one hour. Profiles use mode `0700` under
the OS temporary directory. Set `ABLER_BROWSER` or pass
`--browser /path/to/chromium` to choose a browser. `--keep-browser` is a
debugging exception that leaves the browser and credential-bearing profile
open. Login uses only a private `--remote-debugging-pipe` over inherited file
descriptors; no debugging port is opened. If that pipe cannot be established,
login closes the browser, removes the profile, and suggests `abler-mcp auth
capture` or `abler-mcp auth import` instead. The retained browser's pipe closes
when the CLI exits, so it has no debugging endpoint afterwards. With
`--keep-browser` the profile still holds the live Abler cookies in the browser's
own storage, outside the encrypted store: close the browser and delete its
`abler-login-*` profile when done (the next login removes it after an hour).

### Capture from Chrome

Use this when Chrome is already running with a debugging port or you need a
browser other than the automatically discovered one.

Start a separate Chrome profile with a debugging port bound to loopback. For example, on macOS:

```sh
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --user-data-dir="$HOME/.config/abler-mcp/chrome" \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port=9222 \
  https://www.abler.io/sign-on/login
```

On Linux, use your Chrome/Chromium executable with the same flags. The separate profile is required by [Chrome's current debugging rules](https://developer.chrome.com/blog/remote-debugging-port).

Sign in normally, leave an Abler tab open, then run in another terminal:

```sh
abler-mcp auth capture http://127.0.0.1:9222
```

This captures the Abler session, retains only its `refreshToken` and `id_token` cookies, verifies renewal and API access, and saves them. Close that Chrome instance after capture. The MCP server does not use the debugging port.

### Import an existing browser session

If your browser tooling can export cookies, import its JSON directly:

```sh
abler-mcp auth import /private/path/cookies.json
```

Accepted formats are a cookie array or an object with a `cookies` array, including Playwright storage state and CDP `Network.getCookies` output. Browser exports with `expirationDate` are also accepted. Only the two Abler auth cookies are retained; cookies for other sites and analytics are ignored.

The export must be a regular, single-link file owned by you with owner-only permissions (`chmod 600` on Unix), in a private directory. Symlinks, hard links, shared permissions, and files larger than 4 MiB are rejected. `abler-mcp auth import -` reads JSON from stdin with the same 4 MiB limit.

With a browser that only provides developer tools, open **Application → Cookies → https://www.abler.io**, copy the `refreshToken` value into a private JSON file, and import it:

```json
{
  "cookies": [
    {
      "name": "refreshToken",
      "value": "PASTE_THE_COOKIE_VALUE_HERE",
      "domain": "www.abler.io",
      "path": "/"
    }
  ]
}
```

These cookies are HttpOnly, so `document.cookie` cannot export them. Treat the JSON as a credential; keep it out of source control and chat, and remove the temporary export after a successful, verified import. A failed verification keeps the previous session and retains the new one as an encrypted candidate, because Abler may already have rotated the imported credential: `Session verification failed. The previous session was kept; the new one is retained in the encrypted store. Run abler-mcp auth retry-candidate, or capture a fresh session.` `abler-mcp auth retry-candidate` verifies the retained candidate again and uses it if it works. If Abler reports the new session expired or revoked, that message is shown instead; retrying cannot help, so sign in again. A store failure shows its own message. A later successful login, capture or import replaces it; no plaintext `.pending` file is written.

### Move to a server without a browser

Sign in on the headless machine itself: install the package there, copy a fresh
browser cookie export to it privately, and run `abler-mcp auth import` there. The
saved session cannot be copied between machines (its key stays on the machine that
created it). Do not reuse one session on two machines: Abler rotates the refresh
cookie, so the copies would invalidate each other. Sign in separately for each
host. `auth logout` forgets only this package's local session, without signing out
other devices.

Processes on one machine coordinate refresh, reads, imports, and logout with file
locks: `session.enc.lock` beside the encrypted record, and, for login, import,
retry-candidate, migrate and logout, `session.json.lock` beside the plaintext
session path. A request waits up to 30 seconds for a busy lock and then fails with
a busy error; retry later.
After a hard process crash, the lock is released as soon as its PID no
longer exists. A live process is never expired based on the lock's age, including
while suspended. If the OS reuses a crashed owner's process ID for another live
program, the lock can remain busy; remove it with `rm -r <file>.lock` only when no
process is using the session. Do not remove an active lock: a request whose lock is
taken away fails and must be retried. Temporary files left behind by a crash are
removed after five minutes. Network filesystems and multi-host lock behavior are not
supported or verified. Browser sessions can expire or be revoked: capture/import
again when Abler rejects renewal.

### Where the session is saved

The session (the `refreshToken` and `id_token` cookies, rewritten after every
rotation) and any candidate awaiting verification are saved only as one encrypted
record (AES-256-GCM), `session.enc`, with a non-secret
`session.enc.marker` beside it. The record's 256-bit key is created on the first
login, capture, import or migrate. It is never regenerated, except when the key is
gone and you sign in again with `auth login`, `auth capture` or `auth import`.

The key is a `0600` file in its own `0700` directory, apart from the record:

| Platform | Record and marker                                                | Key                                                                   |
| -------- | ---------------------------------------------------------------- | --------------------------------------------------------------------- |
| macOS    | `~/Library/Application Support/family-mcp/abler-mcp/session.enc` | `~/Library/Application Support/family-mcp/keys/abler-mcp.default.key` |
| Linux    | `~/.config/abler-mcp/session.enc`                                | `~/.local/share/family-mcp/keys/abler-mcp.default.key`                |

On Linux, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` replace `~/.config` and
`~/.local/share`; set the same values for sign-in and the MCP host. On macOS they do
not move the store. On macOS both store directories are excluded from Time Machine
before any secret is written in them, and every start confirms it. Linux has no
standard for this: leave `~/.local/share/family-mcp/keys` out of your backups. A
power cut during a save can lose that change or leave the store unreadable; the
server then says so, and you sign in again. It never uses a damaged session.

Every start except `--help` and `--version` checks the store before anything else.
If a store file or directory could be read or replaced by another user (permissions
that let others in, another owner, a link instead of a real file or folder, a folder
above it that others can write to, or extra sharing permissions on macOS), or Time
Machine did not confirm that it skips the store, `abler-mcp` prints
`abler-mcp: cannot start.` with what is wrong, the path, and, for most problems, the
command that fixes it, and exits; it never changes permissions for you.

An earlier test build kept this store under `~/.config` on macOS, with the key in
the macOS Keychain or under `~/.local/share`. That store is not used. At start the
server lists the old files with the exact commands to remove them; run
`abler-mcp auth login` first, then remove them.

Rotated cookies are written to the record under its lock before any further
request, also when Abler answers with an error.

`abler-mcp auth status` verifies the session and says how it is saved (`storage`).
`abler-mcp auth logout` forgets the session and any candidate on this computer; the
key and the record stay.

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

### Upgrading from 0.6.0

Versions up to 0.6.0 saved the session in a plaintext file,
`~/.config/abler-mcp/session.json` or `ABLER_SESSION_FILE`, and kept failed-import
candidates beside it as `<file>.<uuid>.pending`. That file keeps working,
rotations included, until you migrate, and `auth status` says `Saved in a plaintext
file. Run abler-mcp auth migrate.` Stop running `abler-mcp` servers, then run:

```sh
abler-mcp auth migrate
```

It saves the session in the encrypted record, reads it back, and removes the
plaintext file and its `.pending` candidates. Without a plaintext session it moves
the newest readable `.pending` candidate into the record instead and asks you to
run `abler-mcp auth retry-candidate`. Running it again says `Already migrated.` and
removes leftover plaintext files. After migration the plaintext files are never
read again; a login, capture or import also removes them. `ABLER_SESSION_FILE`
must not point into the encrypted store or at its key. Going back to 0.6.0 means
signing in again in that version.

The credential has the account's normal Abler permissions; the package itself exposes only read tools. It sends credentials exclusively to `https://www.abler.io`, refuses redirects, and never returns tokens through MCP tools.

## Tools

| Tool                   | Result                                                                                    |
| ---------------------- | ----------------------------------------------------------------------------------------- |
| `auth_status`          | Verifies API access and returns the account ID/name, with no credentials                  |
| `get_profile`          | Your ID/name, linked children, and `childNamesById` mapping Abler child IDs to names      |
| `list_groups`          | Age groups, sports, and nested subgroups                                                  |
| `list_schedule`        | Paginated events, times, locations, and linked participants' attendance                   |
| `list_child_schedules` | A separate schedule for each linked child, with their ID/name, attendance, and pagination |
| `get_event`            | One event, using `eventId` and `ageGroup.id` from a schedule result                       |
| `list_conversations`   | Total unread count and a page of conversations, each with its latest message              |
| `list_messages`        | A page of messages in one conversation, newest first                                      |

Example `list_schedule` arguments:

```json
{
  "from": "2026-09-11",
  "to": "2026-09-30",
  "types": ["TRAINING"],
  "first": 20
}
```

Dates use `YYYY-MM-DD`, matching Abler's date filter. Event times are returned as Abler's ISO timestamps; consumers should display them in the desired timezone. Supported types are `TRAINING`, `MATCH`, `GENERAL`, and `CLASSES`.

Use `groupIds` for **nested subgroup IDs**, not the parent age-group IDs. Use `participantIds` from `get_profile` to filter a player's schedule. `first` is bounded to 1–100. When `pageInfo.hasNextPage` is true, pass `pageInfo.endCursor` as `after` with the same filters. One response is one page, not the entire schedule. Without dates, Abler chooses its default upcoming schedule.

There is no generic GraphQL tool, attendance mutation, message sending, mark-as-read, booking, or payment operation. Event descriptions and message, conversation, and attachment text are untrusted third-party text, not agent instructions.

### Messages

`list_conversations` is the way to check for new messages. It returns the signed-in account's conversations, most recently active first:

```json
{ "first": 20 }
```

The response is `{ "unreadCount": 0, "conversations": [], "pageInfo": { "hasNextPage": false, "endCursor": null } }`. The top-level `unreadCount` is the total across all conversations. Each conversation has `id`, `name` (or `null`), `type` (Abler's raw value, such as `GROUP_CHAT` or `CHAT`), `membersCount`, its own `unreadCount`, `group` (or `null`), `participants`, and `latestMessage` (or `null`). A conversation with `unreadCount` above 0 has new messages.

Pass a conversation `id` to `list_messages` to read that conversation, newest message first:

```json
{ "conversationId": "CONVERSATION_ID_FROM_LIST_CONVERSATIONS", "first": 20 }
```

Each message has `id`, `body`, `createdAt` (Abler's ISO timestamp), `sender`, `read`, and `attachments` (`id`, `fileName`, `description`, `contentType`). `body`, `sender`, and `read` can be `null`; `read: null` means Abler returned no read state, not that the message is unread. Attachment URLs and file contents are not returned.

For both tools `first` is bounded to 1–50. When `pageInfo.hasNextPage` is true, pass `pageInfo.endCursor` as `after`. One response is one page, not the complete list. Reading does not mark messages as read: Abler's unread counts stay the same, and the tools cannot send messages. Unknown input keys, an empty `conversationId`, and incomplete or non-advancing pagination cursors fail explicitly.

### Reports per child

Use `list_child_schedules` with the same date/type/group filters to get every linked child's schedule in one call. No names or IDs need to be supplied for the default family report:

```json
{ "from": "2026-09-11", "to": "2026-09-30", "first": 20 }
```

The response is `{ "children": [{ "child": { "id": "…", "displayName": "…" }, "events": [], "pageInfo": { "hasNextPage": false, "endCursor": null } }] }`. Each entry identifies the child explicitly, including children with no events in the requested range. An account with no linked children returns `children: []`. Use the stable ID to distinguish children who share a name; display the name in reports.

`first` applies **per child**. Each child's page is fetched with Abler's participant filter, so one child's events cannot crowd another child out of a combined page. Shared events appear in each relevant child's schedule. Each event's `attendance` contains only that child's records, retaining Abler's raw `status` and `coachStatus` codes; an empty attendance array means no matching record was returned, not that the child is absent.

`get_profile` includes an explicit key/value lookup, fetched fresh from Abler:

```json
{ "childNamesById": { "ABLER_CHILD_ID_A": "Alex", "ABLER_CHILD_ID_B": "Alex" } }
```

The keys are **Abler's assigned IDs**, not locally generated IDs. The values are current display names, so duplicate names remain separate entries. Use the keys in `childIds` (or `participantIds` for `list_schedule`); filtering never uses a name or array position. This map is returned alongside the profile's `children` array and is not persisted as a second, potentially stale child directory.

Use optional `childIds` from `get_profile` to select children. To continue a child's schedule, pass just their ID in `childIds` and their cursor in `afterByChild`, keeping the original filters:

```json
{
  "from": "2026-09-11",
  "to": "2026-09-30",
  "childIds": ["CHILD_ID_FROM_GET_PROFILE"],
  "afterByChild": { "CHILD_ID_FROM_GET_PROFILE": "THAT_CHILDS_END_CURSOR" }
}
```

Unknown input keys, empty participant/child/group filters, unknown child IDs, and non-advancing pagination cursors fail explicitly. Follow each child's `hasNextPage` before reporting their schedule as complete. Failed requests return an error rather than an empty schedule.

## What was verified

On **2026-09-11**, against an authorized existing account:

- Browser OTP sign-in issued HttpOnly `refreshToken` and `id_token` cookies.
- A browser-independent `POST /oauth/token` returned HTTP 201 and updated both cookies. Access-token lifetime was 600 seconds. The captured refresh cookie initially expired about 80 days later; this is an observation, not a guaranteed lifetime.
- Cookie-authenticated `POST /graphql` fetched profile/group data and practices. Date and training filters, pagination, and event lookup were verified against live results.
- Per-child filtering was verified for all three children on the account, including children with no events in the requested range.
- The website sends OTP initiation to `/auth/graphql` with an `x-recaptcha-token` header. The package uses normal browser login; unattended SMS initiation is not implemented.
- Google sign-in is offered by Abler's UI; its resulting session is intended to use the same cookie import route. A separate Google login was not tested.

These are observed website endpoints, not a documented public API contract. GraphQL introspection is disabled. Abler's sign-in bundle also contains OAuth consent UI with `offline_access`, but public client registration and schedule access through that flow were not established. An approved Abler OAuth integration would be the route to investigate for a future distributed service.

The Chrome capture transport is covered by a local protocol test; live session capture in this investigation used the browser's CDP tool. Live API proof is separate from the offline tests.

## Troubleshooting

| Symptom                                          | Action                                                                                                                                                                                                                                                               |
| ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `abler-mcp` not found                            | Use the absolute installed executable path printed by the installer.                                                                                                                                                                                                 |
| Server appears to wait in the terminal           | Server mode waits for MCP input on stdio. Use an MCP host, or run `--help` / `auth status` for a CLI response.                                                                                                                                                       |
| No saved session                                 | Sign in with the same `ABLER_SESSION_FILE`, and on Linux the same `XDG_CONFIG_HOME` and `XDG_DATA_HOME`, that the MCP host uses.                                                                                                                                     |
| Saved in a plaintext file                        | Run `abler-mcp auth migrate`.                                                                                                                                                                                                                                        |
| Cannot read session / unsafe permissions         | Before migration: use a regular file owned by your user, not a symlink or hard link; run `chmod 600 /absolute/path/session.json` on Unix. Its parent must be writable for atomic rotation and locking. Windows is unsupported by the encrypted store.                |
| `abler-mcp: cannot start.`                       | The store is unsafe, or Time Machine did not confirm that it skips it. The next lines name the path and, for most problems, the command that fixes it (for example `chmod 700`, `chmod 600`, or `tmutil addexclusion`). Nothing is changed for you.                  |
| Leftover of an earlier test build                | That build kept the key in the macOS Keychain. Remove `session.enc` and `session.enc.marker` from `~/Library/Application Support/family-mcp/abler-mcp`, then run `abler-mcp auth login` again.                                                                       |
| The Abler store key is missing                   | The key was deleted. The record cannot be decrypted; `abler-mcp auth login` (or capture/import) replaces it with a new key and record.                                                                                                                               |
| The last write … did not complete                | An interrupted write left `session.enc` and `session.enc.marker` inconsistent. Remove both files (see the table above), then sign in again.                                                                                                                          |
| Cannot use the Abler session store               | Run `abler-mcp auth status` in a terminal: an unsafe store gets what is wrong and its path there. Otherwise the record, marker, or key is damaged or from another key; nothing is reset automatically. Restore the key, or remove the record and marker and sign in. |
| Expired or revoked session                       | Sign in again and capture/import. A refresh token is not permanent.                                                                                                                                                                                                  |
| Session busy                                     | Requests already wait up to 30 seconds; retry when the active request ends. A crashed process releases its lock when its PID no longer exists. A reused PID can keep a lock busy; remove it with `rm -r <file>.lock` only when no process is using the session file. |
| Capture cannot list tabs                         | Launch a separate Chrome profile with the documented flags, keep it open, and use the exact loopback port.                                                                                                                                                           |
| No Abler tab found                               | Open and sign in at `https://www.abler.io` in that debugging profile.                                                                                                                                                                                                |
| Failed import with a retained candidate          | Run `abler-mcp auth retry-candidate`. If it fails again, capture a fresh session.                                                                                                                                                                                    |
| Abler rejects a query or returns unexpected data | Check inputs and account access. Retry transient connection errors; an upstream API change may require a package update. Failures are not empty schedules.                                                                                                           |
| HTTP 429 / service error                         | Back off and retry later. No automatic retry loop is implemented for service errors.                                                                                                                                                                                 |

## Development and packaging

Maintainers use the pinned Bun 1.4.2 toolchain. Oxlint, Oxfmt, the type-aware
lint engine, and TypeScript are pinned in the repository root. Consumers run the
standalone Bun executable.

The server uses the [official MCP TypeScript SDK](https://ts.sdk.modelcontextprotocol.io/v2/get-started/first-server), native `fetch`, `tough-cookie` for cookie handling, and the private `@family-mcp/session-store` workspace package for locking, owner-only files and the encrypted session record (its README states the security contract). No browser dependency or download is required.

From the monorepo root:

```sh
bun install
bun run check
bun run test
bunx turbo run test:binary test:installer --filter=abler-mcp --force
```

Offline tests include MCP stdio, Chrome protocol, and loopback HTTP fixtures.
Build/release output and the Bun lockfile are excluded from formatting; runtime
dependencies are excluded from linting.

Use `bun run lint:fix` for safe lint fixes and `bun run format` to format files
and sort imports. `bun run lint`, `bun run format:check`, and
`bun run typecheck` can also run independently.

The rules reject unused code, unsafe `any` and type assertions, unhandled
promises (including `void` escapes), redundant classes and generics, unnecessary
conditions, import cycles, and nesting deeper than four levels. Bun's
`test`/`it`/`describe` focused, skipped, and placeholder methods are forbidden.
Unused lint suppressions fail the check; TypeScript error expectations need an
explanation. Use existing functions and platform APIs before adding wrappers.

Intentional exceptions: cookie updates and child requests may run sequentially;
authentication errors omit original causes that could expose credentials;
console output is allowed only in the CLI and package smoke check. Do not
weaken rules or hide failures to make a check pass. Linter and formatter settings
live in [`.oxlintrc.json`](../../.oxlintrc.json) and [`.oxfmtrc.json`](../../.oxfmtrc.json).
See the official [Oxlint type-aware guide](https://oxc.rs/docs/guide/usage/linter/type-aware)
and [Oxfmt configuration reference](https://oxc.rs/docs/guide/usage/formatter/config-file-reference).

Every tool declares a strict output schema and returns validated `structuredContent`; upstream fields outside those schemas are discarded. Offline tests use synthetic credentials and include real MCP stdio, local Chrome protocol capture, concurrent processes, logout during refresh, failed-import recovery, migration, encrypted-store errors, private file permissions, invalid filters, malformed pagination, sibling ID/name collisions, shared events, and a loopback HTTP fixture. See the [review record](docs/REVIEW.md) for verified scope and remaining limitations.

This project is not affiliated with or endorsed by Abler.

`bun run build:binary` creates a standalone executable and checksummed archive
for the current machine in `release/`. It uses [Bun's compiler](https://bun.com/docs/bundler/executables),
with automatic `.env` and `bunfig.toml` loading disabled. Configuration comes
from the process environment. GitHub Actions builds and checks macOS 15 and
Ubuntu 24.04 on both CPU architectures, including a copied executable running
outside the checkout without the repository toolchain.

Installer regressions run as `test:installer`; native MCP smokes run as
`test:binary`. The shared builder generates notices from the actual bundle. Run
`release:sync` after editing the package version; CI checks that generated
installers and documented URLs are current.
