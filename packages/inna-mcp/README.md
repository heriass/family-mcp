# Inna MCP (preview)

An unofficial local MCP for Inna school accounts. It reads a student's
timetable, assignments/homework, assignment descriptions, course assessment,
attendance, material metadata, messages, announcements, and absence history.
Whole-day illness registration and leave applications are an explicit opt-in.
A guardian's session can read [several students](#several-students).

The current preview is `inna-mcp@0.4.1`. The read endpoints
were captured in a real guardian account. Electronic-ID login and private session
reuse were verified through the initial 0.1.0 compiled native CLI; the 0.1.1
parsing and repeated-read changes are checked offline. Absence creation is based
on the delivered Inna client and has not been submitted live. See the
[endpoint and login investigation](../../docs/analysis/inna-mcp-endpoints.md).

## Install

The native installer supports macOS and glibc Linux on arm64/x64, verifies the
release checksum, and installs `~/.local/bin/inna-mcp`. No Node, npm, or Bun is
needed at runtime.

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/inna-mcp@0.4.1/packages/inna-mcp/install.sh | sh
```

Upgrading replaces the command but not a running server. Restart the MCP host,
or rerun the installer with `--stop-running`. The installer does not change
saved school sessions or absence-operation records. After upgrading from 0.3.0
or earlier, run `inna-mcp auth migrate` once (see
[Upgrading from 0.3.0](#upgrading-from-030)).

To build before publication, use the pinned Bun 1.4.2 and the Rust toolchain that
`rust/rust-toolchain.toml` pins, from the repository root:

```sh
bun install
bun run --cwd packages/inna-mcp build:binary
```

The standalone executable is `packages/inna-mcp/release/native/inna-mcp`.

### Electronic ID

```sh
inna-mcp auth login
inna-mcp auth status
```

Enter the Icelandic phone number at the hidden prompt. The CLI immediately
prints the four-digit security code before initiating phone verification. Any
agent running this CLI must show the exact code to the user, including leading
zeros, before waiting for approval. Match it on the phone and approve there;
enter the electronic-ID PIN only on the phone. Never send the PIN in chat.

The CLI uses the captured HTTP flow without a browser. Opaque identity-provider
tickets and Inna access tokens stay in memory. Only verified school cookies are
saved, in the same encrypted record as after a cookie import. Login stops
when additional device verification or new terms acceptance requires browser
interaction; it never accepts terms.

Several student contexts no longer stop login (this requires a release after
Inna 0.2.2). The first student context Inna lists becomes the default student;
when a session is already saved, its default student is chosen again, so a
fresh login does not change the default. With `--allow-account-change` the
first listed student becomes the new default. Reach the other students with
`inna_list_students` and `studentKey`. Login still stops when Inna lists no
accessible student context.

### Google

Sign in with a Google account already linked in Inna, on a machine with a
desktop and Google Chrome or Chromium:

```sh
inna-mcp auth login --google
inna-mcp auth status
```

A browser window opens on Inna's Google sign-in. Sign in there; when Inna's
student page appears, the CLI saves the session and closes the window. Nothing
is copied or pasted, and an agent running this command never asks for, reads,
or copies cookies or passwords. This requires a release after Inna 0.2.2. It is
checked offline against a fake browser and has not yet been run against Inna
and Google.

The window is a fresh temporary browser profile, mode `0700`, so it is not
signed in to Google and holds no saved passwords. The CLI talks to it only over
a private `--remote-debugging-pipe`; no debugging port is opened. It waits until
a tab shows `nam.inna.is/Components/Students/Students.html` and the school
session cookies exist, takes only the `SESSION`, `JSESSIONID`, and `XSRF-TOKEN`
cookies of `nam.inna.is`, confirms the browser is closed, removes the profile,
and only then verifies and saves the session. If closure cannot be confirmed,
login exits with an error, saves nothing, and still removes the profile. Each
login also removes abandoned `inna-login-*` profiles older than one hour.

`--timeout <seconds>` changes the five-minute wait for sign-in. `--browser
<path>` or `INNA_BROWSER` selects the browser executable when Chrome or Chromium
is not found by itself. If sign-in is not finished in time, including when
Google refuses to sign in in this window, nothing is saved; run the command
again, or use electronic ID. On Linux without `DISPLAY` or `WAYLAND_DISPLAY`
the command stops before opening anything; use electronic ID there, or the
cookie import below.

### Cookie import, for a machine without a desktop

Use this only when neither electronic ID nor `auth login --google` can run on
the machine. Sign in through [Inna](https://www.inna.is/) in a browser
elsewhere and export only the `nam.inna.is` cookies to an owner-only
local JSON file, as an array or `{ "cookies": [...] }`; supported fields are
`name`, `value`, `domain`, `path`, `secure`, `httpOnly`, `expires` or
`expirationDate`, and `sameSite`. Keep the export outside the repository and
never send cookie values, identity numbers, or PINs in chat.

```sh
chmod 600 /absolute/path/inna-cookie-export.json
/absolute/path/inna-mcp auth import /absolute/path/inna-cookie-export.json
/absolute/path/inna-mcp auth status
/absolute/path/inna-mcp serve
```

Every login path verifies the account/student/school before saving. The student
selected at login or import becomes the session's default student. A later
login or import that lands on a different binding is refused unless the owner
deliberately adds `--allow-account-change`; when it lands on another student
already read through this session, select the default student in the Inna
browser session first. Replacing the default forgets the other learned students.
No unattended login or renewal is implemented. Expiry needs a fresh explicit
login/import.

### Where the session is saved

The session (the `nam.inna.is` cookies, rewritten after every request, with the
default and learned student bindings and any rate-limit pause) is saved only as
one encrypted record (AES-256-GCM), `session.enc`, with a
non-secret `session.enc.marker` beside it. The record's 256-bit key is created on
the first login, import or migrate. It is never regenerated, except when the key
is gone and you sign in again with `auth login` or `auth import`.

The key is a `0600` file in its own `0700` directory, apart from the record:

| Platform | Record and marker                                               | Key                                                                  |
| -------- | --------------------------------------------------------------- | -------------------------------------------------------------------- |
| macOS    | `~/Library/Application Support/family-mcp/inna-mcp/session.enc` | `~/Library/Application Support/family-mcp/keys/inna-mcp.default.key` |
| Linux    | `~/.config/inna-mcp/session.enc`                                | `~/.local/share/family-mcp/keys/inna-mcp.default.key`                |

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
Machine did not confirm that it skips the store, `inna-mcp` prints
`inna-mcp: cannot start.` with what is wrong, the path, and, for most problems, the
command that fixes it, and exits; it never changes permissions for you.

An earlier test build kept this store under `~/.config` on macOS, with the key in
the macOS Keychain or under `~/.local/share`. That store is not used. At start the
server lists the old files with the exact commands to remove them; run
`inna-mcp auth login` first, then remove them.

Windows is unsupported by the encrypted store.

`inna-mcp auth status` verifies the session and says how it is saved, as does
`inna_session_status` (`storage`). `inna-mcp auth logout` forgets the session on
this computer; the key, the record and the absence record stay.

The [absence record](#whole-day-illness-and-leave) is not part of the encrypted
record. It stays a plaintext owner-only file, `session.json.absence.json` in
`~/.config/inna-mcp/`, or `<INNA_SESSION_FILE>.absence.json` when that variable
is set. It holds the prepared request (dates, reason, student and school ids),
no cookies. Login, import, migrate and logout never change or remove it.

Processes on one machine coordinate with two file locks, always taken in this
order: `session.json.lock` beside the plaintext session path, which also guards
the absence record, then `session.enc.lock` beside the encrypted record. A
request waits up to 30 seconds for a busy lock and then fails; retry later. If
Inna changed the cookies and they cannot be written back, the record is removed
so the old cookies are never offered again: the next command reports that the
last write did not complete, and you sign in again. When the cookies did not
change, the record is kept and the command reports the store error.

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

| Message                                | Action                                                                                                                                                                                                                                                              |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Saved in a plaintext file              | Run `inna-mcp auth migrate`.                                                                                                                                                                                                                                        |
| `inna-mcp: cannot start.`              | The store is unsafe, or Time Machine did not confirm that it skips it. The next lines name the path and, for most problems, the command that fixes it (for example `chmod 700`, `chmod 600`, or `tmutil addexclusion`). Nothing is changed for you.                 |
| Leftover of an earlier test build      | That build kept the key in the macOS Keychain. Remove `session.enc` and `session.enc.marker` from `~/Library/Application Support/family-mcp/inna-mcp`, then run `inna-mcp auth login` again.                                                                        |
| The Inna store key is missing          | The key was deleted. The record cannot be decrypted; `inna-mcp auth login` or `auth import` replaces it with a new key and record.                                                                                                                                  |
| The last write … did not complete      | `session.enc` and `session.enc.marker` disagree, or the record was removed after a failed write. Remove both files (see the table above), then sign in again.                                                                                                       |
| Cannot use the Inna session store      | Run `inna-mcp auth status` in a terminal: an unsafe store gets what is wrong and its path there. Otherwise the record, marker, or key is damaged or from another key; nothing is reset automatically. Restore the key, or remove the record and marker and sign in. |
| INNA_SESSION_FILE overlaps the … store | Point `INNA_SESSION_FILE` away from the encrypted record, its marker and lock, and the key file.                                                                                                                                                                    |

Never remove the absence record to fix a session-store message.

### Upgrading from 0.3.0

Versions up to 0.3.0 saved the session in a plaintext file,
`~/.config/inna-mcp/session.json` or an absolute `INNA_SESSION_FILE` path. That
file keeps working, written back after every request as before, until you
migrate, and `auth status` says `Saved in a plaintext file. Run inna-mcp auth
migrate.` Stop running `inna-mcp` servers, then run:

```sh
inna-mcp auth migrate
```

It saves the session in the encrypted record, reads it back, and removes the
plaintext session file. The absence record beside it is left exactly as it is.
Running it again says `Already migrated.` and removes a leftover plaintext
session file. After migration the plaintext session file is never read again; a
login or import before migration also moves the session into the record and
removes the file. Keep `INNA_SESSION_FILE` set as before if you used it: it still
names where the absence record is. It must not point into the encrypted store or
at its key. Going back to 0.3.0 means signing in again in that version.

### Keeping the session alive

Inna issues browser session cookies only; there is no refresh token. A new session
needs your phone or Google. While the native `inna-mcp serve` runs, it verifies
the session with a small authenticated request every ten minutes and saves any
rotated cookies. It reads no school data and never switches the selected student.
A successful, validated user response with usable session cookies counts as a
renewal; cookie values and failures never count as proof of session lifetime.

The renewal age limit is thirty minutes, measured in memory with a monotonic
clock. Before a school request uses an unknown or older session, it must verify
it first. Restarting the server makes the renewal age unknown, so the first idle
verification runs after one minute, including with `--no-keep-alive`. Skipped or failed
ticks retry after one minute; rate-limit pauses still prohibit requests. A
network, service or store failure can prevent renewal, so the server fails school
calls closed until verification succeeds. Transient failures retain the session;
a sign-in refusal gives the existing re-sign-in message and pauses idle requests
until the saved credentials change. The background server also reports a fixed,
credential-free message when renewal is blocked past the bound.

A read answered with 401 reloads the saved session and makes one verification
request, then retries that read once. A second refusal fails
closed. The absence submission POST is never retried: a failed response keeps
its outcome uncertain, as before. A student switch answered with 401 also recovers
once, then reports the existing switch-refused text.

`inna-mcp serve --no-keep-alive` now disables only the optional ten-minute touches.
Mandatory renewal still runs every twenty minutes while serving, including when
no tools arrive. Both schedules stop when the connection closes. Outside `serve`,
nothing renews the session automatically. A host that starts the server per
conversation leaves the session idle between conversations; there, schedule
`inna-mcp auth status` (for example from cron) for the same effect.

These timing and retry guarantees were checked offline with synthetic short-lived
sessions. Whether real Inna extends its session on these requests, and its actual
session lifetime, remain unmeasured. The owner can run the read-only school check
in [SESSION-CHECK.md](docs/SESSION-CHECK.md) for more than seventy minutes. `auth
status` verifies the default student and may switch Inna's selection back to it;
no absence is submitted.

### Connect an MCP host

Use the absolute installed binary path. A normal read-only configuration is:

**Claude Desktop**

```json
{
  "mcpServers": {
    "inna": {
      "command": "/absolute/path/to/.local/bin/inna-mcp",
      "args": ["serve"]
    }
  }
}
```

**Claude Code**

```sh
claude mcp add inna -- /absolute/path/to/.local/bin/inna-mcp serve
```

**Codex**

```toml
[mcp_servers.inna]
command = "/absolute/path/to/.local/bin/inna-mcp"
args = ["serve"]
```

An absolute `INNA_SESSION_FILE`, `XDG_CONFIG_HOME` or `XDG_DATA_HOME` may be set
in the host's private environment; use the same values for sign-in. On macOS the
XDG variables do not move the encrypted store.
Auth login/import, migrate and logout
exist only in the CLI; untrusted school text cannot invoke them through MCP.

## Tools

| Tool                     | Inputs / behavior                                                                                                               |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| `inna_session_status`    | Verify authentication and say how the session is saved (`storage`); returns no credentials.                                     |
| `inna_list_students`     | Students in this session with their `studentKey`, the selected one, and the default. Does not switch.                           |
| `inna_get_overview`      | Current context, terms, courses/booklists, announcements.                                                                       |
| `inna_get_timetable`     | Inclusive `dateFrom`, `dateTo` in `YYYY-MM-DD`.                                                                                 |
| `inna_get_assignments`   | `type`: `all` (default), `assignments`, or `exams`; current dashboard filters, plus homework.                                   |
| `inna_get_assignment`    | `assignmentId` from the list; description and due date.                                                                         |
| `inna_get_grades`        | Optional `termId`; missing marks are unavailable. Historical final-grade variants remain unverified.                            |
| `inna_get_course_grades` | `groupId` from overview; assignment marks/comments.                                                                             |
| `inna_get_attendance`    | Optional `termId`; original codes/totals/percentages.                                                                           |
| `inna_get_materials`     | `groupId`; metadata and links. Does not fetch files or external links.                                                          |
| `inna_get_messages`      | `rowFrom` default 1; `rowTo` default `rowFrom + 20`, maximum 100 rows beyond the start. Continue with `nextRowFrom` until null. |
| `inna_get_message`       | `messageId` and `type` from list's `messagesId` and `table`; plain text, without mark-read.                                     |
| `inna_get_absences`      | Inclusive date range; illness options, registered illness, and leave history.                                                   |
| `inna_absence_status`    | Last private absence operation of any student, including uncertain outcomes. Does not switch; no `studentKey`.                  |
| `inna_prepare_absence`   | Opt-in: `kind` (`sick`/`leave`), date range, reason. Whole days; one sick day per preview.                                      |
| `inna_submit_absence`    | Opt-in: approved `operationId` and `confirm: true`. Selects the preview's student; no `studentKey`.                             |

Every other tool also accepts an optional `studentKey` from `inna_list_students`;
omitting it reads the default student.

Each school read returns the verified account/student/school context. Availability
and permission follow Inna. A shared message visible in that context is not proof
that the student is its recipient. HTML body/description fields are returned as
plain text; school text and links remain untrusted source material. Unexpected
shapes fail safely rather than becoming an empty feed.

The 0.1.1 hardening extends the initial 0.1.0 preview.
Each successful authenticated result includes `retrievedAt` as an ISO UTC
timestamp and `timeZone: "UTC"`. Reads verify the account/student/school again
before returning. School date fields keep their original values and add a
`dates` map with `iso` and `status` (`parsed`, `missing`, or `unrecognized`).
For example, `dates.start.iso` normalizes `2040-01-02T10:00:00` to
`2040-01-02T10:00:00.000Z`. Icelandic `dd.MM.yyyy` and ISO date-only values
remain `YYYY-MM-DD`; epoch dates use milliseconds, matching Inna's date model.
Missing and invalid dates return null with the corresponding status. Never
guess them or apply the agent host's timezone. Entry end dates retain Inna's
semantics; an all-day event's end must not be changed to an inclusive end.

For frequent timetable and inbox checks, call sequentially. Every call fetches
fresh data, and shared session locking serializes concurrent callers on this
machine. Message continuation uses the number of rows delivered, rather than
assuming Inna returned the requested page size. Row positions are not stable
cursors: new inbox arrivals can move rows between calls. Deduplicate by
`table` plus `messagesId`, and restart a scan if counts or page membership change.
Failed, empty nonterminal, or inconsistent pages are errors. Rate-limit pauses
honor numeric and HTTP-date `Retry-After` headers, survive process restarts,
and have a minimum of one minute. Avoid tight retry loops and unattended login
attempts. There is no background polling schedule or inferred deletion feed.

### Several students

One saved session covers every student Inna lists for the signed-in guardian.
Call `inna_list_students`, then pass a returned `studentKey` to the read tools
or to `inna_prepare_absence`. Without `studentKey` a tool reads the default
student: the one selected at `auth login` or `auth import`.

Inna keeps a single selected student per session, so the server switches it
when a call asks for another student and switches back on the next call for
the default. Before reading, it requires that Inna reports exactly the
requested student as selected and that the returned user and school match that
entry. The first verified read of a student records its account/student/school
binding in the saved session; later reads must return the same binding,
and two keys can never share a student. The context is checked again after the
read, and any mismatch discards the result. Always identify the returned
`context` before describing records.

Poll students sequentially. A switch also changes what an open Inna browser
session using the same cookies shows, and a switch made in that browser is
corrected on the next call. `inna_list_students` returns the student names Inna
lists; treat them as untrusted school text. Identity numbers and Inna's
access links are never read, stored, or returned.

Because a call can change Inna's selected student, every tool that accepts
`studentKey` is annotated as not read-only. It still changes no school record.
Only `inna_list_students` and `inna_absence_status` are annotated read-only.

A user confirmed switching between two students on a real account on
2026-10-03: the list returned two keys, and an overview read succeeded for the
default student and the sibling, at different schools. The switch request comes
from Inna's delivered student application and one two-student user's capture;
the tests use synthetic responses.

### Whole-day illness and leave

```sh
/absolute/path/inna-mcp serve --allow-absence-writes
```

Prepare the exact student, school, kind, dates, and reason. Show the returned
preview and get explicit human approval to transmit these details to the school
through Inna. Only then submit its `operationId` with `confirm: true`; this flag
represents MCP-host consent, not a separate authentication mechanism.

The preview records its student. Submission takes no `studentKey`: it selects
that student itself and refuses if Inna's selection changes during its checks.
`inna_absence_status` reports the operation whichever student is selected.

The preview expires after ten minutes. Submission rechecks account context,
permissions, and overlapping records. A returned ID means the request was
submitted, not that the school approved leave. Partial-day requests, cancellation,
messages, assignment submission, and grade editing are unsupported.

The private `session.json.absence.json` marker, a plaintext owner-only file
[outside the encrypted record](#where-the-session-is-saved), is persisted before
a write.
`submitting` or `unknown` means the outcome needs owner review against Inna's
history. The server refuses replay and new previews, for every student, while
that state remains.
Logout retains the marker; do not delete or change it merely to retry. Existing
overlapping records are conservatively refused without interpreting undocumented
status codes.
Sick history is checked for both illness and leave requests. Unrecognized
history dates, a changed UTC day, or expiry during final checks stop submission.
The stored preview's `expiresAt` is epoch milliseconds.

## Development

```sh
bun run --cwd packages/inna-mcp test
bun run --cwd packages/inna-mcp check
bun run --cwd packages/inna-mcp test:binary
bun run --cwd packages/inna-mcp test:installer
```

The release executable is built from the Rust crate in `rust/inna-mcp`
(`familyMcp.release.rust`), which serves the same tools, texts, CLI, sign-ins,
encrypted store, plaintext migration and absence record as the TypeScript
sources here. From `rust/`:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
FAMILY_MCP_BUN="$(command -v bun)" cargo test --all-features --locked
```

The Rust tests run this package's integration, sign-in, browser and startup
cases against the binary, and compare it with the TypeScript CLI and MCP server
on the same synthetic inputs. They also check that either implementation reads
the store and the absence record the other writes, and submits a preview the
other prepared. Only the `test-origin` build that tests use sends requests to a
loopback fake. The release build sends them only to the HTTPS hosts
`nam.inna.is`, `r.inna.is`, `heimdallur.inna.is`, `inna.is` and
`innskra.island.is`; Google sign-in happens in the browser window, not in the
executable. Archives include the license notices of every statically linked
crate.

Tests use synthetic responses only. A separately authorized live native check
verified phone login, private session reuse, and the 13 read/status tools of
0.1.1 using an isolated temporary session. A user confirmed switching between
two students on a real account on 2026-10-03. Real absence submission, a
measurement of whether the keep-alive extends a session, and persistent
registration in an MCP host have not been performed.

See the [release process](docs/RELEASING.md) for packaging and publication gates.
