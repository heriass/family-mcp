# Huawei V261a-20 MCP — Phase 1

Read-only FastMCP server for the Nova Iceland ISNNOVAFTTR router at
`https://192.168.1.1`. The server uses stdio; the Home Assistant add-on wraps it
with mcp-proxy on port 8095. Review branch only: nothing has been installed or
run against the home router. No Phase 2 write tools are included.

## Tools

| Tool | Result |
| --- | --- |
| `router_status` | Model, firmware, uptime, WAN state/IP/uptime and available traffic counters |
| `router_devices(name_filter="")` | Clients sorted online first, case-insensitive name filter; IP, MAC, port/band, connected time |
| `router_ports` | LAN links, speed, byte/packet counters |
| `router_wifi` | Primary and guest enabled state, SSID, band and security mode; no passwords |
| `router_parental` | Current rule/template/device/schedule constructor records |
| `router_access_rules` | Precise access/MAC rules and DHCP reservations |
| `router_logs(kind="firewall", limit=50)` | Firewall or user records; global limit 1–1000 across constructor groups |
| `router_dump_page(name)` | Allowlisted page source, only with `ROUTER_DISCOVERY=1` |
| `router_diagnose()` | One login, deviceinfo read/parse and logout; step names, HTTP codes and byte lengths only |

The single generic parser reads `function Name(fields)` and `new Name(args)`;
it decodes JS string escapes without evaluating JavaScript. Normalized fields
use documented aliases in `server.py`. Missing fields are `null`; counters and
times preserve router units/values. Sanitized original fields remain in
`details` so firmware differences do not silently discard information. Unknown
online status remains `null`, and no frequency band is guessed from a port ID.
WAN dynamic records and WAN list records are both retained, not falsely joined.

Every output removes field names matching
`pass|pwd|psk|key|token|secret|wpa|wep|csrf`, recursively. Embedded credential-like
log assignments and exact environment credentials (including base64) are
scrubbed. All tools return valid JSON within 60,000 characters; oversized results
return an Icelandic error, never truncated JSON.

Discovery returns **redacted source**, not constructor JSON. It conservatively
masks all JS string/numeric literals, HTML text, comments and attribute values;
form identifiers, code structure and simple ASP/CGI action paths remain. Known
sensitive values are masked even when they resemble a path. Scripts containing
unsupported template/regex literals are entirely redacted. This intentionally
removes benign values too. Rendered layout, exact string arguments and arbitrary
write payloads cannot be reconstructed from this output. The source is review
material and must not be executed as a substitute for the router page.

## Authentication and session lifetime

Credentials come only from `ROUTER_USERNAME` and `ROUTER_PASSWORD`; there are no
CLI credential arguments or credential files. `ROUTER_HOST` defaults to the HTTPS
address above. The HA wrapper maps its configured options into those variables
in memory; Home Assistant itself manages `/data/options.json`.

The attached login page's `Submit()`/`LoginSubmit()` implementation was inspected.
For ISNNOVAFTTR each call:

1. Creates a fresh HTTP session and POSTs `/asp/GetRandCount.asp`.
2. Sets `Cookie=body:Language:english:id=-1`.
3. POSTs `/login.cgi` with `UserName`, base64 `PassWord`, `Language=english`, and
   `x.X_HW_Token` set to the returned nonce. No SHA transformation or login retry.
4. Reads only the requested allowlisted status pages.
5. Attempts logout in `finally`, including failed/timed-out login submissions,
   HTTP/parsing errors and discovery calls, then clears cookies and closes HTTP.

Concurrent calls in one server process are serialized across the whole session.
Do not run multiple copies against this single-session router. The owner's web UI
can still conflict with an active call; use the tools when the UI is idle.
Redirects are never followed, so credentials cannot be forwarded to another
host. Login-page responses and login redirects are errors. HTTPS certificate
verification is disabled for this local self-signed router, and ambient HTTP
proxies are disabled. No request bodies, credentials or page sources are logged.

**Hardware verification still required:** the supplied login HTML has no logout
handler or authenticated status samples. Logout uses the Huawei ONT route
`POST /logout.cgi?RequestFile=html/logout.html`, corroborated by an
[independent ONT client](https://github.com/loiklo/huawei-onu-to-graphite/blob/main/huawei-onu-to-graphite-http.py).
This is an inference for the V261a-20, not a firmware-specific confirmation.
A failed logout is recorded in the safe stderr diagnostics but is non-fatal:
it cannot replace a successful read or hide the original login/page error.
Confirm that the owner's web UI is usable after a call during eventual live
review. Exact status aliases, Wi-Fi band representations and all status/log
constructor shapes also need real firmware samples. No live router test has
been performed and no router credentials were requested or used.

## Local verification

From this folder, in an isolated Python environment:

```sh
python -m pip install -r requirements.txt
python sync_addon.py
python -m unittest discover -s tests -v
```

The suite covers parser escaping (including Unicode and backslashes), opaque
strings/comments, non-execution of expressions, recursive redaction, discovery,
limits, normalized mappings, HTTP failures/timeouts, one login attempt, logout,
concurrency, all read tools, add-on copy parity and a real MCP stdio handshake.
All HTTP router tests use mocks and synthetic records, not captured secrets.

## Home Assistant add-on wrapper

`addon/` is a standalone HA build context: `config.yaml`, Dockerfile, `run.sh`,
and generated copies of `server.py`, `router_parser.py`, `requirements.txt`.
After editing canonical files run `python sync_addon.py`; tests enforce parity.
The Dockerfile uses `debian:trixie-slim`, `python3-venv`, pinned Python packages
and mcp-proxy 0.13.0. Shell files are checked out with LF endings.

Options: `host`, `username` (default `Useradmin`), `password` (password schema),
`discovery` (default false). Internal port: `8095/tcp`, with no published host
port. Boot: auto; architecture: amd64. The reviewed TCP watchdog stays enabled.
The wrapper runs exactly:

```sh
mcp-proxy --pass-environment --host 0.0.0.0 --port 8095 -- python server.py
```

The proxy offers `/sse` and `/mcp/`; it does not add authentication. Port 8095
belongs on the trusted home network. The selected proxy CLI and HTTP MCP
handshake were tested locally. A Debian container build and actual HA startup
were not run because Docker/HA are unavailable in this workspace. For a later
reviewer with Docker, the build context is `addon/` (not the repository root):

```sh
docker build -t huawei-router-mcp:review addon/
```

No installation, deployment, reboot, Wi-Fi change, parental-control change or
device blocking was performed. Phase 2 requires a separate approved branch.

## Safe live diagnostics (0.2.2)

Each attempted HTTP step emits exactly one JSON line to stderr, after response
validation/parsing. Fields are `step` (`nonce`, `login`, `page:<name>`, `logout`),
`status`, `location` and `length` (response bytes). A failed step also includes
`exception`, containing only its class name. No exception messages, tracebacks,
request/response bodies, cookies, nonce, credentials or base64 credentials are
logged. Status and length are `null` if no HTTP response was received.

Location diagnostics omit authority, query and fragment. Only known static
router paths are shown; other paths are `[redacted]` because a session secret
could be embedded in the path itself. Redirects are still never followed.
Existing successful same-router login redirects remain accepted; rejected login
redirects now produce a specific Icelandic error with the login step and code.

HTTP status failures, timeouts, SSL/connection errors, returned login pages and
parser ValueErrors now return specific Icelandic errors. Parser errors identify
the page, never the parser message or page source. Unexpected step failures
also log only the exception class and return a fixed error naming the step.

Call `router_diagnose()` to perform one login attempt, read and parse deviceinfo,
and attempt logout. It returns `steps` with only `step`, `status`, `length`;
it includes a safe top-level `error` if login/read fails. Logout is included even
if login/read fails after login submission. Logout failures are logged and shown
by their step metadata but remain non-fatal, preserving the reviewed behavior.
No diagnostic requires discovery mode or returns parsed router data.
