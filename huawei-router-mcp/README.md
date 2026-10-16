# Huawei V261a-20 MCP — Phase 1 review scaffold

**NOT READY TO INSTALL.** The login page HTML containing Submit()/LoginSubmit() is unavailable. The authentication function in server.py intentionally fails closed. Its logout implementation is a placeholder, not a confirmed router logout request. No router was contacted.

Included: FastMCP stdio tools, constructor parser, field-name secret filtering, output limit, tests, and an HA add-on wrapper. The tools cannot read live data until exact authentication/logout is implemented.

**Differences from requested scope:** router_dump_page emits sanitized parsed constructor data instead of raw HTML; status/port/device outputs retain constructor keys rather than a fully mapped normalized schema; no verified login/logout. These changes are intentionally conservative to avoid unsafe credential/session handling.

Run tests locally:
```sh
cd huawei-router-mcp
python3 -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
python -m unittest discover -s tests -v
```

Before installation: provide redacted login-page JS, implement and mock-test exact login/logout sequence including failed HTTP reads, review certificate and network exposure of port 8095, validate field mapping and redaction with real sample pages. Do not proceed with Phase 2 write tools until separately approved.
