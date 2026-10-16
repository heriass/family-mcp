"""Read-only Huawei router MCP server. Authentication intentionally fails closed pending verified login JS."""
from __future__ import annotations

import json
import os
import re
from contextlib import contextmanager
from functools import wraps
from urllib.parse import urlparse

import requests
from mcp.server.fastmcp import FastMCP

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
SENSITIVE = re.compile(r"pass|pwd|psk|key|token|secret|wpa|wep", re.I)
LIMIT = 60000
FUNCTION = re.compile(r"\bfunction\s+([A-Za-z_$][\w$]*)\s*\(([^()]*)\)")
CONSTRUCTOR = re.compile(r"\bnew\s+([A-Za-z_$][\w$]*)\s*\(")


def js_arguments(source: str, start: int) -> tuple[list[object], int]:
    """Read JS constructor arguments without eval; supports quoted strings and numeric literals."""
    parts, buf, quote, escape, depth = [], [], None, False, 0
    i = start
    while i < len(source):
        c = source[i]
        if quote:
            buf.append(c)
            if escape:
                escape = False
            elif c == "\\":
                escape = True
            elif c == quote:
                quote = None
        elif c in "'\"":
            quote = c
            buf.append(c)
        elif c == "(":
            depth += 1
            buf.append(c)
        elif c == ")":
            if depth:
                depth -= 1
                buf.append(c)
            else:
                if buf or parts:
                    parts.append("".join(buf).strip())
                return [decode_literal(p) for p in parts], i + 1
        elif c == "," and depth == 0:
            parts.append("".join(buf).strip())
            buf = []
        else:
            buf.append(c)
        i += 1
    raise ValueError("Ólokið JavaScript-fall í svari beinis.")


def decode_literal(value: str) -> object:
    if len(value) >= 2 and value[0] in "'\"" and value[-1] == value[0]:
        s = value[1:-1]
        def escape(match: re.Match) -> str:
            v = match.group(1)
            return chr(int(v[1:], 16)) if v.startswith(("x", "u")) else {
                "n": "\n", "r": "\r", "t": "\t", "\\": "\\", "'": "'", '"': '"'
            }.get(v, v)
        return re.sub(r"\\(x[0-9a-fA-F]{2}|u[0-9a-fA-F]{4}|.)", escape, s)
    if value == "null" or value == "undefined":
        return None
    if value in ("true", "false"):
        return value == "true"
    if re.fullmatch(r"-?\d+(?:\.\d+)?", value):
        return float(value) if "." in value else int(value)
    # Never evaluate expressions or expose their raw text (may contain secrets).
    return None


def parse_page(html: str) -> dict[str, list[dict]]:
    fields = {}
    for match in FUNCTION.finditer(html):
        names = [s.strip() for s in match.group(2).split(",")]
        if names and all(re.fullmatch(r"[A-Za-z_$][\w$]*", s) for s in names):
            fields[match.group(1)] = names
    objects: dict[str, list[dict]] = {}
    for match in CONSTRUCTOR.finditer(html):
        name = match.group(1)
        if name not in fields:
            continue
        values, _ = js_arguments(html, match.end())
        objects.setdefault(name, []).append(dict(zip(fields[name], values)))
    return objects


def sanitize(value: object) -> object:
    if isinstance(value, dict):
        return {str(k): sanitize(v) for k, v in value.items() if not SENSITIVE.search(str(k))}
    if isinstance(value, list):
        return [sanitize(v) for v in value]
    if isinstance(value, str):
        # Never surface credential-looking values accidentally embedded in strings.
        value = re.sub(r"(?i)\b((?:password|passwd|pwd|psk|token|secret|wpa|wep|(?:private|api)[_-]?key)\s*[:=]\s*)[^\s,;\"'<]+", r"\1[redacted]", value)
        return value
    return value


def output(value: object) -> str:
    result = json.dumps(sanitize(value), ensure_ascii=False, separators=(",", ":"))
    return result if len(result) <= LIMIT else json.dumps({"error": "Svarið er of stórt (60.000 stafir). Notaðu síu eða lægri mörk."}, ensure_ascii=False)


def router_host() -> str:
    host = os.getenv("ROUTER_HOST", "https://192.168.1.1").rstrip("/")
    parsed = urlparse(host)
    if parsed.scheme != "https" or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path:
        raise ValueError("Ógilt ROUTER_HOST: aðeins https://host[:port] er leyft.")
    return host


class RouterSession:
    def __init__(self, http=None):
        self.http = http or requests.Session()
        self.host = router_host()
        self.http.verify = False  # V261a-20 local, self-signed TLS; review before deployment.
        self.http.trust_env = False  # No ambient proxy credentials.
        self.logged_in = False

    def login(self):
        user = os.environ.get("ROUTER_USERNAME")
        password = os.environ.get("ROUTER_PASSWORD")
        if not user or not password:
            raise RuntimeError("Notandanafn eða lykilorð vantar í viðbótarstillingar.")
        # DO NOT guess the cryptographic/transformation or CSRF sequence.
        # LoginSubmit()/Submit() source has not been supplied or verified.
        raise RuntimeError("Innskráning óvirk: staðfesta þarf LoginSubmit()/Submit() úr beininum.")
    
    def get(self, name: str) -> str:
        if name not in PAGES:
            raise ValueError("Óþekkt síða.")
        response = self.http.get(self.host + PAGES[name], timeout=10)
        response.raise_for_status()
        return response.text

    def logout(self):
        # Authentication must supply the confirmed logout endpoint as well.
        # No speculative request may be sent to a single-session device.
        self.logged_in = False

    def close(self):
        self.http.close()


@contextmanager
def authenticated(session_factory=RouterSession):
    router = session_factory()
    try:
        router.login()  # never retry automatically
        yield router
    finally:
        try:
            router.logout()
        finally:
            router.close()


def read_pages(*names: str) -> dict:
    try:
        with authenticated() as router:
            return {name: parse_page(router.get(name)) for name in names}
    except Exception as exc:
        # Do not expose raw HTTP errors, credentials, pages, or session tokens.
        return {"error": "Ekki tókst að lesa beini. " + (
            str(exc) if isinstance(exc, (ValueError, RuntimeError)) else "Athugaðu tengingu og innskráningu."
        )}


@mcp.tool()
def router_status() -> str:
    """Model, firmware, uptime, WAN state/address and counters where published."""
    return output(read_pages("deviceinfo", "wan", "wanlist"))


@mcp.tool()
def router_devices(name_filter: str = "") -> str:
    """LAN/Wi-Fi clients, online first; optional device name filter."""
    result = read_pages("devices")
    if "error" not in result:
        for key, records in result["devices"].items():
            records[:] = [r for r in records if name_filter.casefold() in str(r.get("HostName") or "").casefold()]
            records.sort(key=lambda r: str(r.get("DevStatus", "")).lower() not in ("1", "online", "up"))
    return output(result)


@mcp.tool()
def router_ports() -> str:
    """Ethernet link/speed and traffic counters."""
    return output(read_pages("eth"))


@mcp.tool()
def router_wifi() -> str:
    """Primary/guest Wi-Fi configuration (all secret-key fields removed)."""
    return output(read_pages("wlan", "guest"))


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
    """Read firewall/user logs, at most 50 entries."""
    if kind not in ("firewall", "user"):
        return output({"error": "Veldu kind=firewall eða kind=user."})
    if limit < 1 or limit > 50:
        return output({"error": "limit verður að vera á bilinu 1–50."})
    page = "fwlog" if kind == "firewall" else "userlog"
    data = read_pages(page)
    if "error" not in data:
        for rows in data[page].values():
            del rows[limit:]
    return output(data)


@mcp.tool()
def router_dump_page(name: str) -> str:
    """Discovery only, disabled by default. Returns sanitized constructor data (not raw HTML)."""
    if os.getenv("ROUTER_DISCOVERY") != "1":
        return output({"error": "Uppgötvunarhamur er óvirkur."})
    if name not in PAGES:
        return output({"error": "Óþekkt síða."})
    # Returning entire raw HTML cannot be proven safe by a regex. Fail closed:
    # only already-parsed and scrubbed data is available for discovery.
    return output(read_pages(name))


if __name__ == "__main__":
    mcp.run(transport="stdio")
