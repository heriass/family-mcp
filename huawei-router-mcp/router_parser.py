"""Conservative parser for Huawei JavaScript constructor payloads.

This module is offline-only. No HTTP requests or credentials are handled here.
"""
import re

SENSITIVE = re.compile(r"pass|pwd|psk|key|token|secret|wpa|wep", re.I)
MAX_OUTPUT_CHARS = 60_000

def _matching(text, at, left="(", right=")"):
    depth, quote, escape = 0, None, False
    for pos in range(at, len(text)):
        c = text[pos]
        if quote:
            if escape:
                escape = False
            elif c == "\\":
                escape = True
            elif c == quote:
                quote = None
        elif c in "'\"":
            quote = c
        elif c == left:
            depth += 1
        elif c == right:
            depth -= 1
            if depth == 0:
                return pos
    raise ValueError("Ólokuð JavaScript-svigi.")

def _split_args(raw):
    parts, start, quote, escape, depth = [], 0, None, False, 0
    for i, c in enumerate(raw):
        if quote:
            if escape:
                escape = False
            elif c == "\\":
                escape = True
            elif c == quote:
                quote = None
        elif c in "'\"":
            quote = c
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == "," and depth == 0:
            parts.append(raw[start:i].strip())
            start = i + 1
    if quote or depth:
        raise ValueError("Ógild JavaScript-gildi.")
    parts.append(raw[start:].strip())
    return parts

def _literal(value):
    value = value.strip()
    if len(value) >= 2 and value[0] in "'\"" and value[-1] == value[0]:
        content = value[1:-1]
        def substitute(m):
            return chr(int(m.group(1), 16))
        content = re.sub(r"\\x([0-9a-fA-F]{2})", substitute, content)
        content = re.sub(r"\\u([0-9a-fA-F]{4})", substitute, content)
        return re.sub(r"\\(['\"\\nrt])", lambda m: {"n":"\n","r":"\r","t":"\t"}.get(m.group(1),m.group(1)), content)
    if value in ("true", "false", "null", "undefined"):
        return {"true":True,"false":False,"null":None,"undefined":None}[value]
    if re.fullmatch(r"-?\d+(?:\.\d+)?", value):
        return float(value) if "." in value else int(value)
    # Do not execute JS or echo arbitrary expressions.
    return None

def parse_constructors(html):
    definitions = {}
    for match in re.finditer(r"\bfunction\s+([A-Za-z_$][\w$]*)\s*\(", html):
        end = _matching(html, match.end()-1)
        fields = [f.strip() for f in _split_args(html[match.end():end])]
        if all(re.fullmatch(r"[A-Za-z_$][\w$]*", f) for f in fields):
            definitions[match.group(1)] = fields
    result = {}
    for match in re.finditer(r"\bnew\s+([A-Za-z_$][\w$]*)\s*\(", html):
        name = match.group(1)
        if name not in definitions:
            continue
        try:
            end = _matching(html, match.end()-1)
            args = [_literal(a) for a in _split_args(html[match.end():end])]
        except ValueError:
            continue
        result.setdefault(name, []).append(dict(zip(definitions[name], args)))
    return result

def sanitize(value):
    if isinstance(value, dict):
        return {k:sanitize(v) for k,v in value.items() if not SENSITIVE.search(str(k))}
    if isinstance(value, list):
        return [sanitize(x) for x in value]
    return value

def bounded(value, limit=MAX_OUTPUT_CHARS):
    import json
    raw = json.dumps(sanitize(value), ensure_ascii=False)
    if len(raw) > limit:
        return json.dumps({"error":"Niðurstaðan er of stór."},ensure_ascii=False)
    return raw

def redact_source(source):
    """Fail closed: never return raw HTML containing auth/session material.

    The source can contain secrets in inline scripts and nonstandard names.
    A safely complete discovery redactor requires verified page samples.
    """
    raise RuntimeError("Óheimilt að birta hráan HTML-kóða án staðfestrar afmáunar.")
