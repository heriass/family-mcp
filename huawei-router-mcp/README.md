# Huawei V261a-20 MCP — incomplete Phase 1 scaffold

**NOT INSTALLABLE.** The requested login-page HTML with `Submit()` and
`LoginSubmit()` was not present in the conversation. Exact ISNNOVAFTTR
non-SHA authentication cannot safely be inferred. There is no router-facing
server or credential handling on this branch yet.

This branch contains an offline-only constructor parser and focused tests.
Neither router login/logout nor the requested FastMCP tools or Home Assistant
add-on are implemented. In particular the discovery source dump fails closed.

To proceed, supply the login page JavaScript **without credentials, session
cookies or CSRF/token values**. Implement one-session login/read/logout with
finally-based cleanup, mock-HTTP tests for logout and login retry limits,
read-only tools, conservative redaction and authenticated MCP access before
installation. Do not expose port 8095 outside a trusted network.

Local offline tests:

```sh
cd huawei-router-mcp
python -m unittest discover -s tests -v
```

Phase 2 write operations are deliberately absent.
