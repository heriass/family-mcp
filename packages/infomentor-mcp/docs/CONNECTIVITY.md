# Direct InfoMentor access: investigation and recommendation

## 18 September 2026: a direct route passed login and MCP reads

**An alternate InfoMentor frontend works from the Grok VM without WARP or a
Tailscale exit node.** Both school hostnames normally resolve to
`213.180.87.183`, which still failed before HTTP over the native VM route.
The mobile app's public API hostname, `api-im.infomentor.net`, resolved to
`213.180.76.9`. Connecting to that address while retaining each original school
hostname for TLS SNI, certificate verification, and HTTP Host worked:

| Native-route check                                       | Result                                                                       |
| -------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `im1.infomentor.is/production/mentor/` at `213.180.76.9` | Verified TLS 1.3, HTTP 200, expected password form and ASP.NET hidden fields |
| `minn.infomentor.is/` at `213.180.76.9`                  | Verified TLS 1.3, HTTP 302 to the expected authentication path               |
| Existing native MCP 0.6.1: full credential login         | Passed in approximately five seconds                                         |
| Separate MCP session check                               | Active; verified account ID saved                                            |
| MCP with a hostname override and no proxy environment    | Session check and `infomentor_get_overview` passed                           |

The certificate covers `*.infomentor.is`. The login and session checks first
used a temporary loopback CONNECT forwarder whose outgoing sockets explicitly
used the VM's native route. TLS remained between the unmodified MCP and
InfoMentor. A second test replaced that forwarder with an `/etc/hosts` entry and
a temporary, destination-specific native-route rule. It passed both CLI status
and the MCP overview tool. This demonstrates an authenticated direct route,
not just a reachable public page.

After these initial checks, the two-host entry was retained on the inspected VM.
Temporary proxies, test processes, and routing rules were removed. Its original
Tailscale exit-node selection and running MCP process were preserved. The saved
network mode at that point was `direct`; WARP was stopped. Existing Tailscale routing may still carry
ordinary traffic, but the controlled checks above did not depend on it.

This is a verified workaround using an alternate frontend, not an advertised
InfoMentor failover contract. Its address and virtual-host configuration may
change. No 24-hour soak test or test on the friend's VM has been completed.
It narrows the network problem to the normal destination/path; it does not
identify the operator responsible for the failures at `213.180.87.183`.

### Reproduce on another Grok VM

First check whether ordinary access already works. A working host does not need
this override. On Linux, release 0.8.0 and later can apply the tested route:

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/infomentor-mcp@0.9.2/packages/infomentor-mcp/install.sh | sh -s -- --with-direct-route
```

The installer requires `/usr/bin/python3` and administrator or `sudo` access.
It resolves `api-im.infomentor.net`, then verifies TLS certificates and the
expected public login responses using the original school hostnames. Only after
both checks pass does it add one marked `/etc/hosts` entry for `im1.infomentor.is`
and `minn.infomentor.is`. It handles no school credentials. This entry applies to
all programs on that machine; no default route or Tailscale setting is changed.

Restart the MCP host after installation. Use the host's existing private
credentials for login and automatic renewal, then check `infomentor-mcp status`
and an overview through the MCP host. No setup tools need to be enabled.

The network selection is saved as `direct-route`. Rerunning the ordinary
installer remembers it and revalidates the current alternate address. It safely
adopts the marked entry from the earlier manual recipe, but refuses to replace
an unmanaged hostname override. Failed network checks leave the hosts file and
previous command unchanged. There is no background address refresh; rerun the
installer if the upstream address changes.

To remove the managed entry and return to ordinary DNS:

```sh
curl -fsSL https://raw.githubusercontent.com/olafurns7/family-mcp/infomentor-mcp@0.9.2/packages/infomentor-mcp/install.sh | sh -s -- --without-direct-route
```

Switching a saved direct-route install to WARP or ordinary direct mode also
removes the managed entry. WARP remains an unreliable alternative on the tested
Grok route, as recorded below.

The release helper was also exercised on the inspected Linux VM over its native
route: TLS preflight, adoption of the manual entry, removal, and reinstallation
passed. The original hosts content and routing rules were preserved afterward.

### A separate session-expiry problem

During the later tests, the VM's session had expired and contained cookies but
no verified `accountId`. Both the existing exit-node route and the alternate
frontend returned the MCP's legacy-session renewal guard. The runtime already
had privately configured credentials. One explicit login using those existing
credentials succeeded through the alternate frontend and saved the verified
account ID; subsequent status and overview checks passed. No account-change
override was used. This repairs the observed prerequisite for automatic
renewal, but does not prove the cause of the friend's roughly 24-hour failure.

### Other alternatives investigated

| Option                  | Current evidence and tradeoff                                                                                                                                                                                                                                                                                                     |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Tor with European exits | A temporary client connected over the native VM route. A Europe-limited run reached Cloudflare FRA; an Iceland-only run used an exit classified as IS by Tor's GeoIP data. Both runs timed out connecting to both school hosts. No school credentials were sent through Tor. Test daemons and routing rules were removed.         |
| Mobile OAuth/API        | `api-im.infomentor.net` is reachable directly. The inspected app's SSO flow returns to the school website; its observed API surface does not establish a replacement for the MCP's timetable/message endpoints. The useful discovery was its alternate frontend address. No device pairing or OAuth token exchange was performed. |
| Tailscale with Mullvad  | Supported selectable regional exit nodes, with no home computer required. The [add-on](https://tailscale.com/docs/features/exit-nodes/mullvad-exit-nodes) requires purchase; current [pricing](https://tailscale.com/pricing) is $5/month per five devices. Not purchased or tested against InfoMentor.                           |
| Proton VPN              | Supports [WireGuard configuration files and server selection](https://protonvpn.com/support/wireguard-configurations), including configurations for free accounts. Requires a separate account and private VPN configuration. Not tested here.                                                                                    |
| Shared European relay   | Can use the MCP's existing `HTTPS_PROXY` support with an authenticated CONNECT proxy restricted to the two school hosts. Requires operating a service and testing its actual egress. No relay was provisioned.                                                                                                                    |

## 18 September 2026 retest: WARP connects, school hosts remain unreachable

**WARP is not currently a dependable unattended connection for Grok Bot users.**
The earlier successful test below is historical. A live retest on the authorized
replacement Grok VM reproduced the reported failure: WARP itself connected, but
could not reach InfoMentor's school service.

The VM runs Debian 13/x64 without systemd. Its installed MCP was 0.6.1, saved in
direct mode, with an existing Tailscale exit node providing its working route.
The installed WARP helper matched this repository's script byte for byte. WARP
was initially stopped; temporarily starting it produced these results:

| Route or check                                             | Result                                                                      |
| ---------------------------------------------------------- | --------------------------------------------------------------------------- |
| WARP daemon                                                | `Connected`, `Network: healthy`                                             |
| Cloudflare trace through WARP                              | `warp=on`, `colo=IAD`, verified TLS                                         |
| WARP tunnel route                                          | Native VM interface to `162.159.198.2:500`, independent of the exit node    |
| Public `www.infomentor.is` through WARP                    | HTTP 200                                                                    |
| `im1.infomentor.is/production/mentor/` through WARP        | Connection timeout before HTTP                                              |
| `minn.infomentor.is` through WARP                          | Connection timeout before HTTP                                              |
| Parent host through SOCKS5 with local DNS                  | SOCKS5 failure, reply code 4                                                |
| Login host with a pinned destination through HTTP CONNECT  | Proxy CONNECT aborted                                                       |
| Both school hosts through the VM's native route            | TCP connects; TLS ends with unexpected EOF after approximately five seconds |
| Both school hosts through the existing Tailscale exit node | Verified TLS; HTTP 200 for login and 302 for the parent entry point         |
| Session check with the running MCP's private environment   | `InfoMentor session is active.`                                             |

Both school names resolved to `213.180.87.183`. Native-route comparisons used
Tailscale's existing bypass mark on individual test sockets; no global routing
change was required. Certificate verification remained enabled. The existing
helper's recovery attempt correctly failed with `WARP could not establish a
verified InfoMentor connection.`

This establishes a destination-specific connectivity failure on the tested WARP
path. Source-network filtering or upstream routing is plausible; the responsible
operator and exact cause require network-side evidence. The earlier successful
WARP test used EWR, whereas this test used IAD. That difference is a lead, not
proof that an edge change caused the failure. The reported roughly 24-hour
lifetime does not establish a WARP expiry timer. The friend's VM was not inspected.

### European WARP route comparison, 18 September

A follow-up test routed only WARP's `162.159.198.0/24` tunnel destination through
the existing Icelandic Tailscale exit node. The same registered client connected
to `162.159.198.2:443`; WARP tunnel statistics reported `Colo: KEF`, and the
Cloudflare trace independently returned `warp=on`, `colo=KEF`, `loc=IS`.
Both school hosts then passed verified HTTPS: HTTP 200 for the login page and
302 for the parent entry point, each in approximately three seconds. These were
anonymous connectivity checks, not another authenticated MCP test.

After restoring the native route and default endpoint selection, the same
client connected to IAD and the Cloudflare trace still succeeded, but both
school requests failed with HTTP CONNECT 502 responses. This comparison
confirms a working Icelandic WARP path and a failing US WARP path in the same
test session. It does not identify which network operator causes the failure.

Setting `warp-cli tunnel endpoint set 162.159.198.2:443` over the native route
did not reproduce KEF: the tunnel remained connecting during a 35-second wait.
The installed client accepts an IP/port override but exposes no country selector.
Cloudflare's [WARP FAQ](https://developers.cloudflare.com/warp-client/known-issues-and-faq/)
describes how network routing affects the selected data center. Its supported
[dedicated egress policies](https://developers.cloudflare.com/cloudflare-one/traffic-policies/egress-policies/)
are Enterprise-only.

The European test therefore still depends on the existing exit node; it is not
a standalone WARP fix for other Grok users. The endpoint override, temporary
routing rule, and test daemons were removed, and the original connection setting
was restored.

### Available WARP settings and further native-route tests

The installed 2026.7.1377.0 client reports `Account type: Free`. Its registered
endpoint list contains `162.159.198.2` and `2606:4700:103::2`, each with ports
443, 500, 1701, 4500, 4443, 8443, and 8095. These are entry addresses and ports,
not a list of regional exits. The same IPv4 address served IAD over the native
route and KEF over the Icelandic exit node. The VM has no native IPv6 default
route, so the IPv6 variants were not tested as independent routes.

| Setting                     | Available control                                                               |
| --------------------------- | ------------------------------------------------------------------------------- |
| Entry endpoint              | `tunnel endpoint set IP:PORT`, or `reset` for automatic selection               |
| Tunnel protocol             | MASQUE or WireGuard; the current local-proxy mode requires MASQUE               |
| MASQUE transport preference | `h3-only`, `h2-only`, or `h3-with-h2-fallback`                                  |
| Operating mode              | `proxy`, `warp`, `warp+doh`, `warp+dot`, `tunnel_only`, or DNS-only `doh`/`dot` |
| Local proxy listener        | `proxy port PORT`; currently loopback port 18443                                |
| Country or city             | No selector in this consumer registration                                       |

Further anonymous tests used the native VM route, without changing the existing
Tailscale exit-node selection:

- Requesting `h2-only` was accepted by the CLI, but the actual tunnel continued
  to report `MASQUE (HTTPS via UDP)` and IAD, including after a daemon restart.
  Both school hosts returned proxy HTTP CONNECT 502 failures. This does not
  establish that an HTTP/2 tunnel was exercised successfully.
- Forcing the registered `162.159.198.2:1701` endpoint with `h3-only` established
  a healthy tunnel; the UDP socket confirmed that port. It still reached IAD,
  and both school requests failed with proxy CONNECT aborts.

Endpoint and transport overrides were reset, test daemons were stopped, and
the original consumer settings, proxy mode, connection setting, and routing
rules were verified restored.

[WARP+](https://developers.cloudflare.com/warp-client/warp-modes/#warp-unlimited)
offers access to a larger network; its documentation does not provide a country
selector. Cloudflare's supported selectable regional egress uses
[dedicated egress IPs and virtual networks](https://developers.cloudflare.com/cloudflare-one/tutorials/user-selectable-egress-ips/),
with the Enterprise egress policies linked above. No such European egress was
provisioned or tested for this consumer registration. The working KEF test
remains dependent on the existing Icelandic exit node.

Authentication is a separate requirement. A session check from an ordinary SSH
shell initially required login; repeating it with the existing MCP environment
succeeded using its configured credentials. Keep credentials available to the
running MCP for [automatic renewal](../README.md#automatic-session-renewal).
Reinstalling WARP cannot repair an expired InfoMentor session.

### Making setup usable by other Grok Bot users

The smallest supported alternative is Grok's **Settings → Computer → Route
egress through this desktop**, followed by the normal direct MCP configuration.
Grok documents this route in its [settings guide](https://docs.x.ai/grok-bot/settings-and-notifications#route-traffic-through-your-desktop).
It depends on that desktop and its network. This feature was documented, not
enabled or live-tested during this investigation.

A setup flow can test both school hosts, configure the existing native MCP, use
the host's private credential input, and verify a session. WARP can remain an
optional route only when those destination checks pass. Its healthy status and
a successful Cloudflare trace are insufficient acceptance checks.

For unattended operation while the desktop is unavailable, a reachable exit
node or an operator-supported repair to the cloud route is still needed.
Grok's [Team Setup](https://docs.x.ai/grok-bot/private-networks) can reapply
installation scripts after computer replacement, but is Enterprise-only and
does not operate or guarantee the networking client. Public setup instructions
must keep each user's credentials private and account-specific.

Two existing implementation limits also matter: the launcher checks tunnel
health rather than school-host reachability, and non-systemd daemon recovery
runs on the next MCP launch, not during an already-running MCP process. Neither
explains away the reproduced failure with a running, healthy tunnel. Switching
to WireGuard is not a compatible proxy-mode fallback; Cloudflare requires
[MASQUE for this mode](https://developers.cloudflare.com/changelog/post/2025-10-07-warp-linux-ga/).

Both temporary WARP daemon instances were stopped after their tests. The saved
direct mode, installed command, running MCP process, and Tailscale exit-node
selection were preserved. The authenticated status check could renew and save
the existing session. No credentials or school records were included in output.
No release, runtime-code change, or new networking service was deployed.

## 11 September 2026 investigation: historical evidence

Investigated on 11 September 2026. Scope: the Icelandic InfoMentor parent site,
this MCP package, and one authorized Grok Bot VM. No passwords, session cookies,
message contents, or pupil names are included in these results.

## Recommendation

The existing Grok Bot VM can run this MCP without a user-operated Tailscale
exit node: **Cloudflare WARP in local proxy mode passed a live test with the
published 0.3.0 executable.** Authentication, overview, message listing, message
details, and notifications all worked. The MCP used only the standard
`HTTPS_PROXY` environment setting; no transport code or dependency was added.

WARP remains a managed network intermediary, so this is not a proxy-free result.
The VM's normal TCP route still fails before HTTP. A proxy-free repair was not
found, and neither rebuilding the VM nor obtaining provider-side traces is part
of the solution. The MCP already works directly from the tested Mac.

The successful WARP test used the VM's normal interface for its tunnel, with
certificate validation enabled for InfoMentor. Temporary routing rules made
that path independent of the configured exit node. Test processes, routing
rules, and the temporary WARP registration were removed afterward. The optional
installer's setup component was subsequently installed with authorization on the
same VM. Its persistent daemon and automatic restart passed live MCP checks.

No tested TLS setting justified a transport change. A different HTTP User-Agent
or automatic retry has no demonstrated benefit for this pre-HTTP failure.

## Verified WARP configuration

Version 0.4.0 adds an optional `install.sh --with-warp` setup for Debian 13/x64.
It installs the verified headless client under `/opt/infomentor-warp`, with only
its daemon libraries; the desktop GUI and browser libraries are not installed.
The installed MCP command starts a healthy local proxy when needed and sets the
proxy environment for its own process. Regular installer upgrades preserve the
choice; `--without-warp` restores the direct MCP command without uninstalling WARP.

The root helper is installed at `/usr/local/libexec/infomentor-warp`. On systemd
hosts it enables `infomentor-warp.service`; on the tested Grok VM it starts the
daemon independently of the SSH session and the MCP launcher restarts it on the
next launch when needed. That restart requires the VM's existing noninteractive sudo access.
The installer does not grant new sudo permissions or change existing WARP
installations. Network preparation adds narrow main-table rules for WARP's proxy
endpoint range and resolved registration API addresses, preserving the global
default route and Tailscale settings.

The persistent setup was tested with the published 0.3.0 binary behind the new
launcher, with no proxy setting supplied by the MCP test client. All nine tools
were listed and session verification, overview, messages, message detail, and
notifications passed. An initial restart exposed a healthy-tunnel status before
the MCP's first request could succeed. Ordinary restarts now reuse the saved
configuration and wait for verified HTTPS to both InfoMentor hosts. After
stopping the owned daemon and removing its three routing rules, the launcher
restored them and all these MCP reads passed with empty standard error.
This validates daemon recovery on the existing VM, not a VM reboot. The systemd
branch is provided for Debian hosts using systemd; live validation here covers
the Grok VM's non-systemd branch.

The tested client was Cloudflare WARP 2026.7.1377.0 for Debian 13, using MASQUE
and a loopback proxy on port 18443. The tunnel connected over the VM's native
interface to `162.159.198.2:500` using QUIC. Its log identified Cloudflare's EWR
location. A tunneled Cloudflare trace returned `warp=on`.

Anonymous InfoMentor requests succeeded through both SOCKS5 and HTTP CONNECT.
The latter is supported by the standalone MCP's embedded Bun runtime:

```sh
# Requires a running WARP daemon and a registered device.
warp-cli mode proxy
warp-cli proxy port 18443
warp-cli connect

# Set this in the environment of this MCP process only.
HTTPS_PROXY=http://127.0.0.1:18443 infomentor-mcp
```

These are the relevant application settings, not a complete daemon installation
procedure. Follow the [official Linux client setup](https://developers.cloudflare.com/warp-client/get-started/linux/)
for installation and registration. This Grok VM does not run systemd, so a
persistent setup must also arrange the daemon's lifecycle on that existing VM.
[WARP local proxy mode](https://developers.cloudflare.com/warp-client/warp-modes/#local-proxy)
limits tunneled application traffic to clients configured to use it.
[Bun documents `HTTPS_PROXY` support](https://bun.com/guides/http/proxy).
The native executable's proxy-environment behavior was not tested here.

The final test downloaded the public Linux x64 release, verified its SHA-256,
then exercised it through MCP with the existing session file. All nine tools
were listed; authenticated overview and all three new data tools passed their
output schemas. Observed message read states stayed unchanged. This was saved
session reuse, not a fresh password login through WARP.

The route check matters: an earlier tunnel initially used the exit node and was
excluded as proof of independence. The accepted run explicitly routed WARP's
actual proxy endpoint range, `162.159.198.0/24`, through the VM's native gateway.
The final run also routed the resolved registration API addresses that way.
The tunnel log confirmed the native source interface. The VM's global default
route, Tailscale exit-node setting, installed MCP command, and session file were
preserved. No VM rebuild, reset, or restart was performed.

## Controlled measurements

Both `im1.infomentor.is` (sign-in) and `minn.infomentor.is` (parent data) resolved
to `213.180.87.183`. The public marketing site, `www.infomentor.is`, resolved to
`213.180.76.16`. Success on the marketing site alone therefore does not prove
the authentication/data service is reachable.

The first comparison ran from the same VM, with the same Python/OpenSSL client,
certificate verification enabled, HTTP/1.1 ALPN, and pinned destination IPs.
Only the route changed. The normal-route probes used Tailscale's existing
bypass routing rule on individual test sockets; the VM's global routes and
exit-node setting were not changed. Requests were anonymous.

| Destination                            | VM normal route                                            | VM exit-node route                                         |
| -------------------------------------- | ---------------------------------------------------------- | ---------------------------------------------------------- |
| `im1.infomentor.is`, `213.180.87.183`  | TCP connected; TLS ended with unexpected EOF after 5.015 s | Verified TLS 1.2; HTTP 200 sign-in page                    |
| `minn.infomentor.is`, `213.180.87.183` | TCP connected; TLS ended with unexpected EOF after 5.017 s | Verified TLS 1.2; HTTP 302                                 |
| `www.infomentor.is`, `213.180.76.16`   | Verified TLS 1.3; HTTP 200                                 | Verified TLS 1.3; HTTP 200                                 |
| `im1.infomentor.is`, forced TLS 1.2    | TLS ended with unexpected EOF after 5.017 s                | Not repeated in this row; default negotiated TLS 1.2 above |

A second comparison separated initial TCP timing from TLS completion and varied
SNI on the same destination. Both tested names are covered by the server's
wildcard certificate; certificate validation remained enabled.

| Route     | Destination IP   | SNI                 | TCP setup | TLS result / total time     |
| --------- | ---------------- | ------------------- | --------- | --------------------------- |
| Normal    | `213.180.87.183` | `im1.infomentor.is` | 15 ms     | EOF / 5,021 ms              |
| Normal    | `213.180.87.183` | `www.infomentor.is` | 16 ms     | EOF / 5,022 ms              |
| Normal    | `213.180.76.16`  | `www.infomentor.is` | 17 ms     | Verified TLS 1.3 / 447 ms   |
| Exit node | `213.180.87.183` | `im1.infomentor.is` | 367 ms    | Verified TLS 1.2 / 1,023 ms |
| Exit node | `213.180.87.183` | `www.infomentor.is` | 294 ms    | Verified TLS 1.2 / 931 ms   |
| Exit node | `213.180.76.16`  | `www.infomentor.is` | 342 ms    | Verified TLS 1.3 / 687 ms   |

The initial timing suggested an intermediary. Later tests established that one
is present: native TCP connections succeeded with an IP TTL of one, and packet
metadata showed a roughly 0.3 ms connection setup followed by acknowledgments
with no TLS response payload and a closure around five seconds later. Delaying
ClientHello by two seconds produced a plaintext HTTP 400 response with
`Server: Pingora`, even though the socket targeted the HTTPS port. This identifies
an intermediary in the observed path; it does not prove which upstream component
caused the original handshake failure.

The VM's existing root service environment contained
`SAND_HTTP_PROXY_NAME=bd-isp-shared-unlimited-us`. This is evidence of a selected
shared proxy path, despite the lack of ordinary shell proxy variables. No
supported control for changing that selection on this existing VM was found.
Setting a similarly named environment variable in the MCP would not establish
that the infrastructure's proxy selection had changed.

Five additional normal-route profiles all closed with the same TLS EOF after
5.019–5.027 seconds: default TLS without ALPN; TLS 1.2 without ALPN; TLS 1.2
restricted to the server's working AES-128-GCM cipher; TLS 1.2 with the P-256
curve; and TLS 1.2 restricted to AES-256-GCM. Certificate and hostname validation
stayed enabled. These tests found no usable compatibility setting to ship.

Reducing TCP maximum segment size to 1200, 1000, and 536 bytes did not fix the
failure. A fresh sandboxed Google Chrome profile also received connection-closed
errors on the native route before any TLS response bytes; a public TLS control
site worked. Splitting ClientHello across TCP writes or TLS records did not
restore the connection. These results cover several concrete compatibility
hypotheses, not every possible TLS fingerprint.

Independent anonymous probes from New York, Nuremberg, and Stockholm all
received HTTP 200 from the same authentication IP. This rules out a blanket
outside-Iceland restriction for those locations, while leaving filtering of
particular source networks possible.
[Public three-location result](https://check-host.net/check-report/4b0b3333k1e3).

Independent anonymous Mac checks succeeded with Node 24.12.0, Bun 1.4.2, and
curl. The authentication host presented a valid wildcard certificate and
negotiated TLS 1.2. Both Bun's default User-Agent and a Safari-style User-Agent
received the sign-in form on that working route. Those checks establish that
these clients can communicate with the service; they do not establish the
reason the VM route fails.

## What the evidence rules out, and what remains open

| Explanation                                                   | Assessment                                                                                                                                                                                                                                                                                          |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| MCP login, form fields, cookie serialization, message parsing | Cannot explain an anonymous TLS failure before HTTP. The earlier empty-cookie saving bug is separately fixed in 0.2.2.                                                                                                                                                                              |
| Different DNS answers between the two routes                  | Excluded for the controlled comparison: both routes used the same pinned destination IP.                                                                                                                                                                                                            |
| HTTP User-Agent rejection                                     | Cannot explain that pre-HTTP failure. An ordinary fresh TLS handshake precedes HTTP application data; User-Agent is an HTTP field. See [TLS 1.3 protocol overview](https://www.rfc-editor.org/rfc/rfc8446#section-2) and [HTTP User-Agent](https://www.rfc-editor.org/rfc/rfc9110#name-user-agent). |
| A missing trusted root certificate                            | No certificate was received on the failing route. The same client verified the server on the working route. Disabling certificate validation has no demonstrated benefit.                                                                                                                           |
| TLS 1.3 negotiation alone                                     | Forcing TLS 1.2 did not fix the VM route.                                                                                                                                                                                                                                                           |
| A filter based only on the tested SNI names                   | Changing SNI while keeping the authentication IP did not restore TLS.                                                                                                                                                                                                                               |
| Shared datacenter source IP blocked downstream                | Plausible, not proven. Grok's [security FAQ](https://docs.x.ai/grok-bot/security-faq) acknowledges that some destinations flag datacenter addresses.                                                                                                                                                |
| Intermediary on the hosted normal path                        | Established by TTL/timing, a Pingora HTTP response, and the proxy-selection environment value. The failing upstream component and reason remain unknown.                                                                                                                                            |
| Blanket restriction on access outside Iceland                 | Excluded for the three independent US, German, and Swedish probes, which all returned HTTP 200.                                                                                                                                                                                                     |

No ordinary `HTTP_PROXY`, `HTTPS_PROXY`, or `ALL_PROXY` environment variable was
present in the inspected VM shell, including lowercase variants. The service
environment and packet metadata establish an intermediary despite that absence.
The leaf certificate on working routes matched, with no observed replacement
certificate. Provider firewall logs were unavailable; no security policy was
disabled.

## Options against the preference for no proxy

| Option                                                   | Proxy-free on the Grok VM?            | Assessment                                                                                                                                                                                 |
| -------------------------------------------------------- | ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Repair the normal hosted path                            | Yes, if successful                    | Theoretical resolution; provider-side traces are unavailable, so this is not the current action plan.                                                                                      |
| InfoMentor approves the hosting provider's source ranges | Yes, if source filtering is the cause | Conditional on operator confirmation and approval, neither available here.                                                                                                                 |
| Alternate supported InfoMentor parent API                | Possibly                              | No independent, documented Icelandic parent API with verified equivalent authentication and data was found. Do not replace working endpoints with a guessed host.                          |
| Run this MCP on a computer whose normal connection works | Direct from that computer             | Already demonstrated on the Mac, but a cloud-only agent still needs a supported way to invoke that local MCP and the computer must be available.                                           |
| Grok's optional desktop egress setting                   | No                                    | The installed desktop bundle labels it “Route egress through this desktop.” It still depends on the desktop and its network. Availability and live operation were not verified or changed. |
| Tailscale exit node or app connector                     | No                                    | A working workaround, with another machine remaining in the route. A narrower app connector can limit routed destinations but still requires a connector host.                             |
| Cloudflare Tunnel to a private MCP                       | No for this requirement               | Exposes or connects to a service elsewhere; it does not repair the VM's direct connection to InfoMentor.                                                                                   |
| WARP local proxy on the existing VM                      | No                                    | Verified with the public 0.3.0 MCP binary through the native VM interface. Persistent setup and recovery through the new launcher also passed.                                             |

Grok documents shared static outbound IP ranges, no dedicated customer IPs, and
destination controls rather than a source-IP selector. It directs customers to
their account team for current ranges. There is no documented source-IP setting
the MCP can select to repair this connection.
[Grok security documentation](https://docs.x.ai/grok-bot/security).

Grok's [settings documentation](https://docs.x.ai/grok-bot/settings-and-notifications)
mentions optional egress controls depending on rollout. The desktop-specific
interpretation above comes from the installed application's own setting label,
not a successful live test. Its [private networking guide](https://docs.x.ai/grok-bot/private-networks)
describes customer-operated network clients. Tailscale's
[exit-node documentation](https://tailscale.com/docs/features/exit-nodes) and
[app-connector documentation](https://tailscale.com/docs/features/app-connectors)
describe the remaining routing dependency.

InfoMentor's official Icelandic [parent handbook](https://www.infomentor.is/wp-content/uploads/2024/03/Handbok-fyrir-adstandendur_september_23_2.pdf)
describes the app as a way into the parent service and advises parents to start
with their school for help. Its public documentation did not establish an
alternate API that would avoid this network path. This is a search limitation,
not proof that no private or partner API exists.

Changing DNS alone would not address the pinned-IP failure above. A native WARP
tunnel was verified instead. Its presence does not identify the original
provider-side fault, but it supplies an independently tested connection option
on the existing machine.

## Message and notification tools

These are implemented in the MCP package and reuse its existing authenticated
HTTP transport, cookie jar, cancellation, account queue, and response validation.
No remote helper or proxy is part of their implementation.

| MCP tool                       | Verified read endpoint                     | Behavior                                                              |
| ------------------------------ | ------------------------------------------ | --------------------------------------------------------------------- |
| `infomentor_get_messages`      | `/Message/message/GetMessages`             | Inbox/sent filters, text search, page and page size.                  |
| `infomentor_get_message`       | `/Message/message/GetMessage`              | Numeric message ID; plain-text body and participants.                 |
| `infomentor_get_notifications` | `/NotificationApp/NotificationApp/appData` | Current notification feed with original states and pupil association. |

The live site's [published application script](https://minn.infomentor.is/dist/scripts/scripts.js)
identifies separate mutation endpoints for marking messages viewed and changing
notification states. The package never calls those endpoints. Reads use form
POSTs accepted by the live service; JSON transport was unnecessary.

Live structural probes verified a populated message list, a full detail,
page-two exhaustion, an unmatched search, and an empty sent folder. The server
honored paging but reported `page: 0`; the MCP reports the requested 1-based
page and preserves the server's `more` flag. A populated sent folder and a
multi-page account have not been verified live.

The notification bootstrap embedded in the parent HTML was empty while the
notification app endpoint returned a populated feed. The tool therefore reads
the latter. Original `Seen` and `Read` states were observed; these are kept
distinct. No complete-history guarantee is inferred from the available feed.

Message read state was compared before and after a detail fetch and was
unchanged. Dedicated live coverage of a previously unread message was not
recorded. The explicit absence
of the site's separate viewed-state request is additionally checked in the
synthetic MCP integration test with an unread message. New/Cleared notification
filtering is also covered synthetically rather than claimed as live evidence.

A live MCP handshake with the built server exposed all nine tools. Calls through
MCP validated message listing, a message detail, the notification feed, and its
selected-child filter. Repeated reads preserved the observed message and
notification states. That first live check used the existing exit route.
A subsequent test with the published standalone executable validated the same
three tools and the overview through native-interface WARP, as described above.
