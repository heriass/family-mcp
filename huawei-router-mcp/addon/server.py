"""Read-only Huawei V261a-20 MCP; one short-lived session per call."""
from __future__ import annotations

import os
import re
import base64
import threading
import json
import sys
from contextlib import contextmanager
from urllib.parse import urlparse

import requests
from mcp.server.fastmcp import FastMCP
from router_parser import parse_constructors as parse_page, sanitize, bounded as output, redact_source, scrub_text

mcp = FastMCP("huawei-router")
PAGES = {
    "deviceinfo": "/html/ssmp/deviceinfo/deviceinfo.asp",
    "wan": "/html/bbsp/common/getWanDynamicData.asp",
    "wanlist": "/html/bbsp/common/wan_list.asp",
    "eth": "/html/amp/ethinfo/ethinfo.asp",
    "devices": "/html/bbsp/common/GetLanUserDevInfo.asp",
    "dhcpstatic": "/html/bbsp/dhcpstatic/dhcpstatic.asp",
    "wlan": "/html/amp/wlanbasic/WlanBasic.asp",
    "guest": "/html/amp/wlanbasic/WlanGuestWifiConfig.asp",
    "parental": "/html/bbsp/parentalctrl/parentalctrlstatus.asp",
    "portacl": "/html/bbsp/portacl/newacl.asp",
    "fwlog": "/html/bbsp/firewalllog/firewalllogview.asp",
    "userlog": "/html/ssmp/userlog/normallogview.asp",
    "reboot": "/html/ssmp/reboot/reboot.asp",
}
def router_host() -> str:
    host = os.getenv("ROUTER_HOST", "https://192.168.1.1").rstrip("/")
    parsed = urlparse(host)
    if parsed.scheme != "https" or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path or not parsed.hostname:
        raise ValueError("Ógilt ROUTER_HOST: aðeins https://host[:port] er leyft.")
    return host


class RouterError(RuntimeError):
    """Only fixed Icelandic messages may cross the tool boundary."""


def location_path(location):
    """Only known static router paths: even a path can contain a secret."""
    try:
        path = urlparse(location).path
    except ValueError:
        return "[redacted]"
    known = set(PAGES.values()) | {"/", "/index.asp", "/login.asp", "/login.cgi",
                                 "/logout.cgi", "/html/logout.html", "index.asp", "login.asp"}
    return scrub_text(path) if path in known else ("[redacted]" if path else "")


class RouterSession:
    def __init__(self, http=None, steps=None):
        self.http = http or requests.Session()
        self.host = router_host()
        self.http.verify = False  # V261a-20 local, self-signed TLS; review before deployment.
        self.http.trust_env = False  # No ambient proxy credentials.
        self.logged_in = False
        self.login_attempted = False
        self.steps = steps if steps is not None else []

    def request(self, method, path, *, step, process=None, **kwargs):
        # Never follow redirects to another host or forward credentials there.
        response, failure = None, None
        try:
            response = self.http.request(method, self.host + path, timeout=10,
                                         allow_redirects=False, **kwargs)
            response.raise_for_status()
            return process(response) if process else response
        except Exception as exc:
            # Never interpolate an exception message, URL, request or response.
            failure = type(exc).__name__
            if isinstance(exc, RouterError):
                raise
            if isinstance(exc, requests.HTTPError):
                if response is None:
                    response = exc.response
                code = response.status_code if response is not None else "óþekktur"
                message = f"HTTP-villa í skrefi {step}: kóði {code}."
            elif isinstance(exc, requests.Timeout):
                message = f"Tími rann út í skrefi {step}."
            elif isinstance(exc, requests.exceptions.SSLError):
                message = f"SSL-villa í skrefi {step}."
            elif isinstance(exc, requests.RequestException):
                message = f"Tengivilla í skrefi {step}."
            elif isinstance(exc, ValueError) and step.startswith("page:"):
                message = f"Ekki tókst að þátta síðu {step[5:]} (ValueError)."
            else:
                message = f"Óvænt villa í skrefi {step}."
            raise RouterError(message) from None
        finally:
            status = response.status_code if response is not None else None
            length = len(response.content) if response is not None else None
            entry = {"step": step, "status": status, "length": length}
            if step == "login":
                # Response.cookies contains only cookies set by this response,
                # not the session jar. Never inspect or serialize their values.
                entry["set_cookie_names"] = sorted({scrub_text(name) for name in response.cookies.keys()}) if response is not None else []
            self.steps.append(entry)
            line = {"step": step, "status": status,
                    "location": location_path(response.headers.get("Location", "")) if response is not None else "",
                    "length": length}
            if failure is not None:
                line["exception"] = failure
            # JSON escapes control characters; one stderr line per HTTP step.
            print(json.dumps(line, ensure_ascii=False), file=sys.stderr, flush=True)

    @staticmethod
    def token_response(response, step):
        # XHR removes the UTF-8 BOM; Response.text can instead decode it as
        # Latin-1 mojibake. Ignore the HTTP charset and decode the raw bytes.
        try:
            token = response.content.decode("utf-8-sig").strip()
        except UnicodeDecodeError:
            raise RouterError(f"Beinir skilaði ekki gildu innskráningarmerki í skrefi {step}.") from None
        if len(token) > 4096 or re.fullmatch(r"[0-9A-Za-z]+", token) is None or 300 <= response.status_code < 400:
            raise RouterError(f"Beinir skilaði ekki gildu innskráningarmerki í skrefi {step}.")
        return token

    def nonce_response(self, response):
        return self.token_response(response, "nonce")

    @staticmethod
    def encoded_password(password):
        # The page's base64encode() uses each char's low byte. Latin-1 matches
        # exactly for U+0000..U+00FF; keep the prior fallback outside that range.
        try:
            raw = password.encode("latin-1")
        except UnicodeEncodeError:
            raw = password.encode("utf-8")
        return base64.b64encode(raw).decode("ascii")

    def login_response(self, response):
        location = response.headers.get("Location", "")
        try:
            rejected = location and (urlparse(location).netloc not in ("", urlparse(self.host).netloc)
                                     or re.search(r"login|error", location, re.I))
        except ValueError:
            rejected = True
        if rejected or (300 <= response.status_code < 400 and not location):
            raise RouterError(f"Óvænt tilvísun frá login.cgi í skrefi login: kóði {response.status_code}.")
        if is_login_page(response.text):
            raise RouterError("Innskráningarsíða kom aftur eftir innskráningu í skrefi login.")

    def verify_response(self, response):
        if 300 <= response.status_code < 400:
            raise RouterError(f"Tilvísun frá index.asp í skrefi verify: kóði {response.status_code}.")
        if is_login_page(response.text):
            raise RouterError("Innskráningarsíða kom aftur frá index.asp í skrefi verify. Innskráningu var hafnað.")
        self.logged_in = True

    def login(self):
        user = os.environ.get("ROUTER_USERNAME")
        password = os.environ.get("ROUTER_PASSWORD")
        if not user or not password:
            raise RouterError("Notandanafn eða lykilorð vantar í viðbótarstillingar.")
        if self.login_attempted:
            raise RouterError("Innskráning var þegar reynd í þessu kalli.")
        # First load the page as the browser does, preserving its session cookies.
        self.request("GET", "/", step="bootstrap")
        form_headers = {"Referer": self.host + "/", "Origin": self.host,
                        "Content-Type": "application/x-www-form-urlencoded"}
        nonce = self.request("POST", "/asp/GetRandCount.asp", step="nonce", process=self.nonce_response,
                             headers={**form_headers, "X-Requested-With": "XMLHttpRequest"}, data={})
        # document.cookie: host-only cookie named Cookie, path /; unrelated
        # cookies from bootstrap/nonce remain in the session jar.
        language_cookie = requests.cookies.create_cookie("Cookie", "body:Language:english:id=-1",
                                                        domain=urlparse(self.host).hostname, path="/")
        language_cookie.domain_specified = False
        self.http.cookies.set_cookie(language_cookie)
        self.login_attempted = True  # Even a timeout may have created a session.
        self.request("POST", "/login.cgi", step="login", process=self.login_response, headers=form_headers, data={
            "UserName": user, "PassWord": self.encoded_password(password),
            "Language": "english", "x.X_HW_Token": nonce,
        })
        # A 200 login.cgi response alone does not establish authentication.
        # Follow only this fixed same-origin navigation, never a supplied URL.
        self.request("GET", "/index.asp", step="verify", process=self.verify_response,
                     headers={"Referer": self.host + "/"})
    
    def get(self, name: str, parsed=False, transform=None):
        if name not in PAGES:
            raise ValueError("Óþekkt síða.")
        def process(response):
            if 300 <= response.status_code < 400:
                raise RouterError(f"Tilvísun í skrefi page:{name}: kóði {response.status_code}.")
            if is_login_page(response.text):
                raise RouterError(f"Innskráningarsíða kom aftur eftir innskráningu í skrefi page:{name}.")
            if transform is not None:
                return transform(response.text)
            return parse_page(response.text) if parsed else response.text
        return self.request("GET", PAGES[name], step="page:" + name, process=process,
                            headers={"Referer": self.host + "/index.asp"})

    def logout(self):
        if self.login_attempted:
            try:
                # Confirmed browser flow: obtain a fresh token before logout.
                form_headers = {"Referer": self.host + "/index.asp", "Origin": self.host,
                                "Content-Type": "application/x-www-form-urlencoded"}
                token = self.request("POST", "/html/ssmp/common/GetRandToken.asp", step="logout_token",
                                     process=lambda r: self.token_response(r, "logout_token"),
                                     headers={**form_headers, "X-Requested-With": "XMLHttpRequest"}, data={})
                def process(response):
                    if 300 <= response.status_code < 400 and not re.search(r'login|logout', response.headers.get("Location", ""), re.I):
                        raise RouterError(f"Óvænt tilvísun í skrefi logout: kóði {response.status_code}.")
                self.request("POST", "/logout.cgi?RequestFile=html/logout.html", step="logout", process=process,
                             headers=form_headers, data={"x.X_HW_Token": token})
            finally:
                self.logged_in = False

    def close(self):
        try:
            self.http.cookies.clear()
        finally:
            self.http.close()


SESSION_LOCK = threading.Lock()


def is_login_page(text):
    return bool(re.search(r'txt_Username|txt_Password|function\s+LoginSubmit|name=[\"\x27]PassWord|location\s*(?:\.href\s*)?=\s*[\"\x27]/?login\.asp', text, re.I))


@contextmanager
def authenticated(session_factory=None):
    # FastMCP dispatches synchronous tools in worker threads.
    with SESSION_LOCK:
        router = (session_factory or RouterSession)()
        try:
            router.login()  # exactly one attempt, no automatic HTTP retries
            yield router
        finally:
            try:
                router.logout()
            except RouterError:
                # Útskráningarslóðin er ekki staðfest á þessum hugbúnaði; mistök þar mega ekki
                # eyðileggja svar sem þegar var lesið (lotan rennur þá út af sjálfu sér).
                pass
            finally:
                router.close()


def read_pages(*names: str) -> dict:
    try:
        with authenticated() as router:
            return {name: router.get(name, parsed=True) for name in names}
    except Exception as exc:
        # Do not expose raw HTTP errors, credentials, pages, or session tokens.
        return {"error": "Ekki tókst að lesa beini. " + (
            str(exc) if isinstance(exc, RouterError) else "Athugaðu tengingu og innskráningu."
        )}


def rows(page):
    return [row for records in page.values() for row in records]


def field(row, *names):
    lookup = {re.sub(r'[^a-z0-9]', '', k.lower()): v for k, v in row.items()}
    for name in names:
        value = lookup.get(re.sub(r'[^a-z0-9]', '', name.lower()))
        if value is not None and value != "":
            return value
    return None


def mapped(page, schema):
    return [{**{key: field(row, *aliases) for key, aliases in schema.items()},
             "details": sanitize(row)} for row in rows(page)]


def first(page, *aliases):
    return next((value for row in rows(page) if (value := field(row, *aliases)) is not None), None)


@mcp.tool()
def router_diagnose() -> str:
    """Browser login/verify, deviceinfo and logout; metadata and login cookie names only."""
    steps = []
    error = None
    try:
        with authenticated(lambda: RouterSession(steps=steps)) as router:
            router.get("deviceinfo", parsed=True)
    except Exception as exc:
        error = str(exc) if isinstance(exc, RouterError) else "Ekki tókst að greina tengingu við beini."
    # Construct output after finally so the logout step is included even on failure.
    result = {"steps": steps}
    if error is not None:
        result["error"] = error
    return output(result)


def _num(value):
    try:
        return int(str(value).strip())
    except (TypeError, ValueError):
        return None


def _bytes64(row, low, high):
    """Huawei splits 64-bit counters into a low 32-bit field and a *_H high field."""
    lo, hi = _num(row.get(low)), _num(row.get(high)) or 0
    return None if lo is None else hi * 2**32 + lo


def _gb(value):
    return None if value is None else round(value / 1_000_000_000, 2)


def _port_no(domain):
    m = re.search(r"LANPort\.(\d+)\.", str(domain or ""))
    return int(m.group(1)) if m else None


SPEEDS = {"0": "10 Mbit/s", "1": "100 Mbit/s", "2": "1 Gbit/s", "3": "2,5 Gbit/s", "4": "10 Gbit/s"}


@mcp.tool()
def router_status() -> str:
    """Router model/firmware, uplink port and total internet traffic since the router last restarted."""
    data = read_pages("deviceinfo", "eth")
    if "error" in data:
        return output(data)
    info = (data["deviceinfo"].get("stDeviceInfo") or [{}])[0]
    eth_rows = rows(data["eth"])
    wan = next((r for r in eth_rows if "BytesReceived" in r and "BytesSent" in r), {})
    uplink = next((r for r in eth_rows if "X_HW_MainUpPort" in r), {})
    return output({
        "model": info.get("ModelName"), "firmware": info.get("SoftwareVersion"),
        "hardware": info.get("HardwareVersion"), "description": info.get("Description"),
        "uplink_port": uplink.get("X_HW_MainUpPort"), "uplink_type": uplink.get("typpon"),
        "internet_total": {
            "received_gb": _gb(_num(wan.get("BytesReceived"))), "sent_gb": _gb(_num(wan.get("BytesSent"))),
            "packets_received": _num(wan.get("PacketsReceived")), "packets_sent": _num(wan.get("PacketsSent")),
            "note": "Teljarar routersins frá síðustu endurræsingu (geta velt yfir við 4 GB á eldri hugbúnaði).",
        },
    })


@mcp.tool()
def router_devices(name_filter: str = "", include_offline: bool = False) -> str:
    """Devices the router knows: name, IP, MAC, wired port (LAN1-4) or Wi-Fi, online status, connected time.
    Online only by default; include_offline=True adds remembered offline devices. LAN2 = UniFi Wi-Fi network."""
    result = read_pages("devices")
    if "error" in result:
        return output(result)
    out = []
    for r in rows(result["devices"]):
        name = r.get("UserDevAlias") or r.get("HostName") or ""
        online = str(r.get("DevStatus", "")).lower() == "online"
        if (not include_offline and not online) or (name_filter and name_filter.casefold() not in name.casefold()):
            continue
        out.append({"name": name or None, "ip": r.get("IpAddr") or None, "mac": r.get("MacAddr"),
                    "port": r.get("Port"), "connection": {"ETH": "snúra", "WIFI": "WiFi"}.get(r.get("PortType"), r.get("PortType")),
                    "online": online, "connected_time": r.get("Time") or None, "ip_type": r.get("IpType") or None})
    out.sort(key=lambda d: (not d["online"], d["port"] or "", (d["name"] or "~").casefold()))
    by_port = {}
    for d in out:
        if d["online"]:
            by_port[d["port"]] = by_port.get(d["port"], 0) + 1
    return output({"count": len(out), "online_by_port": by_port, "devices": out})


@mcp.tool()
def router_ports() -> str:
    """Wired LAN ports 1-4: link, speed, duplex, traffic (GB) and error counters since the router last restarted."""
    data = read_pages("eth")
    if "error" in data:
        return output(data)
    ports = {}
    for r in rows(data["eth"]):
        n = _port_no(r.get("domain"))
        if n is None:
            continue
        p = ports.setdefault(n, {"port": f"LAN{n}"})
        if "Speed" in r and "Status" in r:
            p["link"] = "tengt" if str(r.get("Status")) == "1" else "ótengt"
            p["speed"] = SPEEDS.get(str(r.get("Speed")), r.get("Speed")) if p["link"] == "tengt" else None
            p["duplex"] = {"0": "half", "1": "full"}.get(str(r.get("Mode")), r.get("Mode")) if p["link"] == "tengt" else None
        if "txGoodBytes" in r:
            p["sent_gb"] = _gb(_bytes64(r, "txGoodBytes", "txGoodBytes_H"))
            p["received_gb"] = _gb(_bytes64(r, "rxGoodBytes", "rxGoodBytes_H"))
            p["bad_bytes_received"] = _bytes64(r, "rxBadBytes", "rxBadBytes_H")
        if "RcvFcsErrFrame" in r:
            p["errors"] = sum(_num(v) or 0 for k, v in r.items() if k != "domain")
    uplink = next((r.get("X_HW_MainUpPort") for r in rows(data["eth"]) if "X_HW_MainUpPort" in r), None)
    return output({"uplink_port": uplink, "ports": [ports[k] for k in sorted(ports)],
                   "note": "sent = frá routernum út á gáttina, received = inn á routerinn frá tækinu á gáttinni."})


@mcp.tool()
def router_wifi() -> str:
    """Router's own Wi-Fi networks and guest Wi-Fi: SSID, on/off, hidden, connected devices, security. Never passwords."""
    data = read_pages("wlan", "guest")
    if "error" in data:
        return output(data)

    def nets(page, guest):
        seen, res = set(), []
        for r in rows(page):
            ssid = r.get("ssid") or r.get("SSID")
            if not ssid or "BeaconType" not in r:
                continue
            key = (r.get("domain"), ssid)
            if key in seen:
                continue
            seen.add(key)
            hidden = r.get("wlHide")
            if hidden is None and r.get("SSIDAdvertisementEnabled") is not None:
                hidden = "0" if str(r.get("SSIDAdvertisementEnabled")) == "1" else "1"
            res.append({"ssid": ssid, "enabled": str(r.get("enable")) == "1", "guest": guest,
                        "hidden": str(hidden) == "1" if hidden is not None else None,
                        "devices": _num(r.get("DeviceNum")), "security": r.get("BeaconType"),
                        "band": "5 GHz" if re.search(r"WLANConfiguration\.(5|6|7|8)\b", str(r.get("domain"))) else "2,4 GHz"})
        return res

    return output({"wifi": nets(data["wlan"], False), "guest": nets(data["guest"], True),
                   "note": "UniFi-netið (HeriHeima) er ekki hér; þetta eru net routersins sjálfs."})


@mcp.tool()
def router_parental() -> str:
    """Read parental-control rules/schedules."""
    return output(read_pages("parental"))


@mcp.tool()
def router_access_rules() -> str:
    """Read precise-device-access rules and DHCP reservations."""
    return output(read_pages("portacl", "dhcpstatic"))


@mcp.tool()
def router_logs(kind: str = "firewall", limit: int = 50) -> str:
    """Read firewall/user logs; default 50, maximum 1000 entries across all groups."""
    if kind not in ("firewall", "user"):
        return output({"error": "Veldu kind=firewall eða kind=user."})
    if limit < 1 or limit > 1000:
        return output({"error": "limit verður að vera á bilinu 1–1000."})
    page = "fwlog" if kind == "firewall" else "userlog"
    data = read_pages(page)
    if "error" not in data:
        remaining = limit
        for records in data[page].values():
            del records[remaining:]
            remaining -= len(records)
    return output(data)


@mcp.tool()
def router_dump_page(name: str) -> str:
    """Discovery only: redacted HTML/JavaScript source from an allowlisted page."""
    if os.getenv("ROUTER_DISCOVERY") != "1":
        return output({"error": "Uppgötvunarhamur er óvirkur."})
    if name not in PAGES:
        return output({"error": "Óþekkt síða."})
    try:
        with authenticated() as router:
            source = router.get(name, transform=redact_source)
        return output({"page": name, "source": source})
    except Exception as exc:
        return output({"error": str(exc) if isinstance(exc, RouterError) else
                       "Ekki tókst að lesa eða afmá síðu beinis. Athugaðu tengingu og útskráningu."})


if __name__ == "__main__":
    mcp.run(transport="stdio")
