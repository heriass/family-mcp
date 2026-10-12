# Changelog

## Unreleased

- `serve` now ends when the connection to the host breaks, as the TypeScript
  server does: when a write to stdout fails because the host closed its end,
  or when one input line passes 10 MiB, its newline included. That limit is
  exact; the TypeScript server can also close on a shorter line, depending on
  how the input arrives in reads, which is an intended difference. Calls in
  flight are cancelled at once and the server closes as on SIGTERM; the
  keep-alive stops and its renewal in flight is aborted, then the process
  exits with code 0 and prints nothing. Before, it kept running until stdin
  ended, and it buffered an over-long line without limit.
- After stdin ends, `serve` now also exits within about 6 s while an answer is
  still waiting on a stdout the host has stopped reading: the usual 5 s wait
  for calls in flight, then calls are cancelled and the server closes. Before,
  it could wait for that write forever. The TypeScript server does not exit in
  that case either.

## 0.4.1

- The phone-number prompt of `auth login` is written only once the terminal
  reads keys raw, as in the TypeScript CLI. Before, the prompt could appear
  while the terminal still echoed, so a number typed at once could be shown.
  A terminal that cannot be made raw still fails before any prompt.

## 0.4.0

- The native binary is now built from Rust. Tools, inputs, results, CLI
  commands, messages and exit codes are unchanged except as noted below, as are
  electronic-ID and Google sign-in, cookie import, student
  switching, and the absence preview, submission and record. It was checked
  offline against the TypeScript sources and synthetic Inna, electronic-ID and
  browser fakes; it has not yet been run against Inna.
- The native server measures successful session verification with a monotonic
  clock and gates school requests at a maximum renewal age of thirty minutes.
  Idle verification still runs every ten minutes; failures retry after one minute
  without bypassing rate-limit pauses. Network, service and store failures can
  prevent renewal; affected calls fail closed. Transient errors retain the session.
  Unknown age after restart also requires verification: the first idle tick runs
  after one minute in both modes. No renewal timestamp is
  persisted, and status result fields remain unchanged.
- `serve --no-keep-alive` now disables only the optional ten-minute touches;
  mandatory renewal still runs every twenty minutes without tool calls. A blocked
  renewal reports fixed text without credentials. These are intended deviations
  from the TypeScript scheduler. Real Inna session extension remains unmeasured;
  `docs/SESSION-CHECK.md` gives the owner's seventy-minute check.
- An API read answered with 401 reloads the saved session and makes one
  verification request, then retries the original request once. The TypeScript reference did
  not retry reads. A student switch answered with 401 also recovers once, then
  reports the existing switch-refused text. Absence submission POSTs still never retry, and their uncertain
  outcome contract is unchanged. Offline tests cover short TTLs, blocked renewals,
  one retry, a separate CLI import used by the next call and secret-free whole-run output.
- The session is kept in one encrypted record in the file store, with its key
  in a separate private file: on macOS in
  `~/Library/Application Support/family-mcp`, which is excluded from Time
  Machine; on Linux in `~/.config/inna-mcp`, with the key in
  `~/.local/share/family-mcp/keys`. `inna-mcp auth status` and
  `inna_session_status` show how the session is saved.
- Every start checks the store's location and permissions. A refusal names the
  path and, for most problems, the command that fixes it.
- A session saved by 0.3.0 or earlier keeps working from its plaintext file.
  Stop running servers of the older version, then run `inna-mcp auth migrate`
  once to move it into the encrypted store and remove the file; a login or
  import moves it as well. The absence record stays at
  `<session file>.absence.json`, is never changed by login, import, migrate or
  logout, and keeps blocking as before.
- Sessions saved with earlier test builds that kept the store key in the macOS
  Keychain are not migrated: remove the files the refusal names, then run
  `inna-mcp auth login` again.
- When `auth login` is ended at the hidden phone prompt on a terminal by
  SIGTERM, or by a second SIGINT (the first cancels the sign-in once the line
  ends), it restores the terminal and exits with status 143 or 130. Before, the
  process was killed by the signal, and a second SIGINT left the terminal
  without echo. At the prompt Ctrl-C is a key, not a signal: it cancels the
  sign-in. The hidden prompt takes typed characters, Backspace, Ctrl-U, Enter,
  Ctrl-C and Ctrl-D on an empty line; arrow keys and other cursor movement are
  ignored.
- Pressing Ctrl-C (or sending SIGTERM) while `inna-mcp auth login --google` is
  still starting the browser now gives the browser one more chance to answer
  (up to about 2 s more). If it answers, inna-mcp closes it through its private
  debugging pipe (`Browser.close`) before falling back to SIGTERM. A browser
  that answers but ignores `Browser.close` is signalled after up to 5 s more.
  The temporary profile is still removed and nothing is saved. Messages and
  exit codes are unchanged.

## 0.3.0

- `inna-mcp auth login --google` signs in with Google in one command. It opens
  a temporary Chrome or Chromium window on Inna's Google sign-in, waits for the
  student page, saves the verified session, and closes the window; nothing is
  copied or pasted. `--timeout <seconds>` (default 300) and `--browser <path>`
  or `INNA_BROWSER` are optional. It needs a desktop session. Checked offline
  against a fake browser; not yet run against Inna and Google.
- Cookie import is now documented as the fallback for a machine without a
  desktop.
- Electronic-ID login no longer stops when Inna lists several contexts. It
  signs in to the first student context, or to the already saved default
  student when a session exists, and that student is the default. The other
  students are reached with `inna_list_students`. `--allow-account-change`
  takes the first listed student as the new default. Login still stops when no
  accessible student context is listed.
- Switching between two students was confirmed on a real account by a user on
  2026-10-03: the list returned two keys and an overview read succeeded for the
  default student and the sibling. Absence submission and the effect of the
  keep-alive on session lifetime remain unverified.

## 0.2.2

- While `serve` runs, the server makes one small authenticated request every
  ten minutes so the saved session is not left idle. It saves rotated cookies,
  reads no school data, never switches students, waits out rate-limit pauses,
  and stops after Inna asks for sign-in until the saved session cookies change,
  as after a new login or import.
  `serve --no-keep-alive` turns it off. This only prevents an idle timeout;
  Inna's session lifetime is still unmeasured.
- A student switch that Inna answers with a sign-in request now reports
  "Inna refused the student switch and asked for sign-in" instead of the
  message used for an expired session on an ordinary read.

## 0.2.1

- Tools that accept `studentKey` are no longer annotated read-only, because a
  call can change Inna's selected student for the shared session. They still
  change no school record. `inna_list_students` and `inna_absence_status`
  stay read-only.

## 0.2.0

- Read several students under one saved session. The new `inna_list_students`
  tool returns each student's `studentKey`; the read tools and
  `inna_prepare_absence` accept it and default to the student saved at login or
  import. The server switches Inna's selected student per request and verifies
  the selection, user, school, and learned binding before and after each read.
- Session files are now version 2 and record the verified binding of each
  student read. Version 1 files are read and upgraded on the next use.
- A login or import that lands on another learned student is refused with
  guidance; replacing the default student forgets the learned students.
- Absence previews record their student, submission selects it without a
  `studentKey`, and `inna_absence_status` reports the operation of any student
  of the session without switching. One uncertain operation still blocks new
  previews for every student.
- Live switching between two students is unverified by the maintainer; the
  request is source-verified and covered by synthetic tests only.

## 0.1.1

- Normalize school dates explicitly as UTC, preserve date-only and raw values,
  and expose parsed/missing/unrecognized status plus retrieval timestamps.
- Continue inbox paging using delivered rows; reject incomplete, duplicate,
  and inconsistent pages. Recheck student context before returning reads.
- Preserve numeric and HTTP-date rate-limit pauses across restarts. Shared
  response-body reads now cancel stalled streams on abort; request deadlines
  also cover Inna response bodies.
- Check illness overlap for leave requests and refuse malformed absence dates,
  UTC midnight changes, or preview expiry during final submission checks.
- Exercise all 15 tools offline through MCP, multi-page inboxes, concurrent
  reads, timezone differences, response cancellation, and write refusal paths.

## 0.1.0

- Initial standalone native MCP for Inna school accounts.
- Electronic-ID login uses hidden phone input and approval on the phone. The
  CLI prints the security code before initiating verification; agents must show
  that exact code to the user. Google supports private browser-cookie import.
- Read timetable, assignments/homework, assessment, attendance, material
  metadata, messages, announcements, and absence history in a verified context.
- Whole-day illness and leave requests require opt-in, a private preview, and
  explicit approval. Account/permission checks and persisted operation markers
  prevent replay after uncertain outcomes. Live submission is unverified.
- Shared owner-only session storage, atomic replacement, and locking. No
  unattended renewal, partial-day absence, cancellation, or automatic school
  selection when multiple contexts are available.
