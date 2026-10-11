# Brief: upgrade the family MCP servers on a remote VM to the monorepo releases

> Historical: written for infomentor 0.6.0 / abler 0.5.0. For a current upgrade follow the
> package READMEs (which cover `auth migrate`).

You are operating on a Debian 13 x64 VM (the Grok bot host). InfoMentor MCP is installed
from the old `olafurns7/infomentor-mcp` repository, probably in WARP mode; Abler MCP may be
installed from the old `olafurns7/abler-mcp` repository. Both old repositories are archived.
Move both to the `olafurns7/family-mcp` releases and adapt the bot to the behaviour changes
below. Do not touch session or credential files except where stated. Never print their contents.

## 1. Upgrade InfoMentor (keeps WARP mode automatically)

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/infomentor-mcp@0.6.0/packages/infomentor-mcp/install.sh | sh -s -- --stop-running
```

- Same layout as before: `~/.local/share/infomentor-mcp/<version>-<digest>/`, symlink
  `~/.local/bin/infomentor-mcp`, saved mode in `~/.local/share/infomentor-mcp/network`.
  The installer reads that file, so do NOT pass `--with-warp` again; it is preserved.
- Verify: `~/.local/bin/infomentor-mcp --version` prints `0.6.0`.
- If the VM uses a non-default prefix, set `INFOMENTOR_PREFIX=/absolute/path` before `sh`.

## 2. Upgrade Abler (if installed)

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/abler-mcp@0.5.0/packages/abler-mcp/install.sh | sh -s -- --stop-running
```

- Verify: `~/.local/bin/abler-mcp --version` prints `0.5.0`.
- The VM keeps using the copied session file via `ABLER_SESSION_FILE`. Do not run
  `abler-mcp auth login` here; it needs a browser and is for a laptop.
- Restart the bot after upgrading so its MCP host starts the new binaries.

## 3. Behaviour changes the bot must handle (InfoMentor)

- Setup tools are gone by default. `infomentor_login`, `infomentor_setup_status`,
  `infomentor_cancel_setup`, `infomentor_logout` exist only if `serve` is started with
  `--allow-setup-tools`. Read tools are unchanged: `infomentor_session_status`,
  `infomentor_get_overview`, `infomentor_select_child`, `infomentor_get_messages`,
  `infomentor_get_message`, `infomentor_get_notifications`, `infomentor_collect_updates`.
  If the bot calls any setup tool, either add the flag to the MCP host's `serve` args or
  remove those calls. Prefer removing them; use the CLI for login.
- Existing session keeps working. The legacy path `~/.infomentor-mcp/session.json` is used
  as long as that file exists. Do not move it. (New installs would use
  `~/.config/infomentor-mcp/session.json`.)
- Re-login on the VM: `infomentor-mcp login` (env `INFOMENTOR_USERNAME`/`INFOMENTOR_PASSWORD`
  or `--credentials /absolute/path/credentials.json`, file mode 0600, owned by this user,
  not a symlink or hard link). Replacing a session that belongs to a DIFFERENT account, or an
  unreadable session file, now fails unless `--allow-account-change` is given.
- Output shapes: `infomentor_get_overview`, `_get_messages`, `_get_notifications` and
  `_collect_updates` gained `skipped` (number) and `skippedByFeed`; `displayName` and
  `establishmentName` may be `null`; notification `state` is an open string (known values
  `New`, `Seen`, `Read`, `Cleared`, others pass through). A malformed upstream item no longer
  fails the whole tool; it is dropped and counted. `collect_updates` keeps the previous
  baseline for any feed with skipped rows, so a transient upstream glitch cannot produce
  false "missing" reports.
- Rate limiting: a 429 pause is saved with the session (max one hour) and honoured by every
  process using that session file. Do not retry around `RATE_LIMITED`; wait.
- Locking: the session lock now waits up to 30 s instead of failing instantly with
  "operation in progress"; a scheduled collector and an interactive read no longer collide.
  `login --timeout` now includes that wait.
- Errors: tool errors are fixed literal messages. Do not parse upstream text from them.

## 4. Behaviour changes the bot must handle (Abler)

- All six tools return strict `structuredContent`; unknown upstream fields are stripped
  (`list_groups` is validated now). Errors are fixed literal messages.
- `auth logout` also deletes `.pending` import candidates next to the session file.
- Hard-linked session files are rejected; the file must be a regular 0600 file owned by
  this user.

## 5. WARP on this VM

Unchanged. The launcher `~/.local/bin/infomentor-mcp` (WARP mode) still starts the local
proxy when needed using the existing non-interactive `sudo`. To revert to direct access:
rerun the InfoMentor installer with `| sh -s -- --without-warp`. Reference:
https://github.com/olafurns7/family-mcp/blob/main/packages/infomentor-mcp/docs/CONNECTIVITY.md

## 6. Verify after upgrading

```sh
~/.local/bin/infomentor-mcp --version    # 0.6.0
~/.local/bin/abler-mcp --version         # 0.5.0 (if installed)
```

Then, through the MCP host, call `infomentor_session_status` with `{}` (expect an
authenticated saved session) and, if Abler is installed, `auth_status` with `{}`.
Restart the MCP host so it re-lists tools (InfoMentor now lists seven by default).

Full references: https://github.com/olafurns7/family-mcp#readme,
packages/infomentor-mcp/CHANGELOG.md and packages/abler-mcp/CHANGELOG.md in that repo.
