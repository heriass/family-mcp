"""Non-executing parser and conservative secret removal for router responses."""
import base64
import html
import json
import os
import re
from html.parser import HTMLParser

SENSITIVE = re.compile(r'pass|pwd|psk|key|token|secret|wpa|wep|csrf', re.I)
MAX_OUTPUT_CHARS = 60_000
# Ordered alternatives make quoted strings and comments opaque to the parser.
LEX = re.compile(r'''(?P<comment>//[^\r\n]*|/\*[\s\S]*?\*/)|(?P<string>"(?:\\[\s\S]|[^"\\])*"|'(?:\\[\s\S]|[^'\\])*')|(?P<id>[A-Za-z_$][\w$]*)|(?P<number>-?(?:0[xX][\da-fA-F]+|\d+(?:\.\d+)?(?:[eE][+-]?\d+)?))|(?P<punct>[^\s])''')

def literal(value):
    if value[:1] in ('"', "'") and value[-1:] == value[:1]:
        def escape(m):
            x = m.group(1)
            if x.startswith(('x', 'u')):
                return chr(int(x[1:], 16))
            return {'n':'\n','r':'\r','t':'\t','b':'\b','f':'\f','v':'\v','0':'\0','\n':'','\r\n':''}.get(x, x)
        # Decode once: a literal \\x41 must not become A.
        decoded = re.sub(r'\\(x[\da-fA-F]{2}|u[\da-fA-F]{4}|\r\n|[\s\S])', escape, value[1:-1])
        return decoded.encode('utf-16', 'surrogatepass').decode('utf-16')
    if value in ('true','false','null','undefined'):
        return {'true':True,'false':False,'null':None,'undefined':None}[value]
    try:
        if re.fullmatch(r'-?0[xX][\da-fA-F]+', value):
            return int(value, 16)
        if re.fullmatch(r'-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?', value):
            return float(value) if re.search(r'[.eE]', value) else int(value)
    except ValueError:
        pass
    return None

def tokens(source):
    return [m for m in LEX.finditer(source) if m.lastgroup != 'comment']

def constructor_calls(source):
    ts = tokens(source)
    fields = {}
    for i in range(len(ts)-3):
        if ts[i].group() != 'function' or ts[i+1].lastgroup != 'id' or ts[i+2].group() != '(':
            continue
        j = i+3
        names = []
        while j < len(ts) and ts[j].group() != ')':
            names.append(ts[j].group())
            j += 1
        if j < len(ts) and all(re.fullmatch(r'[A-Za-z_$][\w$]*', x) for x in names[::2]) and all(x == ',' for x in names[1::2]):
            fields[ts[i+1].group()] = names[::2]
    for i in range(len(ts)-3):
        if ts[i].group() != 'new' or ts[i+1].group() not in fields or ts[i+2].group() != '(':
            continue
        name = ts[i+1].group()
        args, buf, depth = [], [], 0
        for t in ts[i+3:]:
            v = t.group()
            if v == ')' and depth == 0:
                if buf or args:
                    args.append(buf)
                yield name, fields[name], args
                break
            if v == ',' and depth == 0:
                args.append(buf)
                buf = []
                continue
            if v in ('(', '[', '{'):
                depth += 1
            elif v in (')', ']', '}'):
                depth -= 1
            buf.append(t)
        else:
            raise ValueError('Ólokið JavaScript-gildi í svari beinis.')

def parse_constructors(source):
    result = {}
    for name, fields, args in constructor_calls(source):
        # Expressions are never evaluated or echoed; missing values remain explicit.
        vals = [literal(a[0].group()) if len(a) == 1 else None for a in args]
        result.setdefault(name, []).append({f: vals[i] if i < len(vals) else None for i, f in enumerate(fields)})
    return result

def scrub_text(value):
    # Also remove exact environment credentials if echoed into a log/hostname.
    for name in ('ROUTER_USERNAME','ROUTER_PASSWORD'):
        secret = os.getenv(name)
        if secret:
            variants = [secret, base64.b64encode(secret.encode()).decode(), html.escape(secret)]
            if name == 'ROUTER_PASSWORD':
                try:
                    variants.append(base64.b64encode(secret.encode('latin-1')).decode())
                except UnicodeEncodeError:
                    pass
            for variant in variants:
                value = value.replace(variant, '[redacted]')
    return re.sub(r'''(?i)\b([\w.$-]*(?:pass\w*|pwd|psk|key|token|secret|wpa|wep|csrf)[\w.$-]*\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;<>]+)''', r'\1[redacted]', value)

def sanitize(value):
    if isinstance(value, dict):
        return {k: sanitize(v) for k,v in value.items() if not SENSITIVE.search(str(k))}
    if isinstance(value, list):
        return [sanitize(v) for v in value]
    return scrub_text(value) if isinstance(value,str) else value

def bounded(value, limit=MAX_OUTPUT_CHARS):
    raw = json.dumps(sanitize(value), ensure_ascii=False, separators=(',',':'))
    return raw if len(raw) <= limit else json.dumps({'error':'Niðurstaðan er of stór (60.000 stafir). Notaðu síu eða lægri mörk.'}, ensure_ascii=False)

def redact_script(source):
    # Discovery deliberately removes ALL runtime literals, including positional
    # constructor secrets and obfuscated field names. Keep code and bare paths
    # so form actions can still be reviewed. Never return executable input.
    forbidden = set()
    for group in parse_constructors(source).values():
        for row in group:
            forbidden.update(str(v) for k,v in row.items() if SENSITIVE.search(k) and v is not None)
    ts = tokens(source)
    for i,t in enumerate(ts):
        key = literal(t.group()) if t.lastgroup == 'string' else t.group()
        if isinstance(key,str) and SENSITIVE.search(key):
            if i+2 < len(ts) and ts[i+1].group() in ('=', ':'):
                value = literal(ts[i+2].group())
                if value is not None:
                    forbidden.add(str(value))
    out, end = [], 0
    for t in LEX.finditer(source):
        out.append(source[end:t.start()])
        if t.lastgroup == 'comment':
            out.append('/* [redacted] */')
        elif t.lastgroup == 'string':
            value = literal(t.group())
            if value not in forbidden and re.fullmatch(r'/[A-Za-z0-9_./-]+\.(?:cgi|asp)', value or ''):
                out.append(json.dumps(value))
            else:
                out.append('"[redacted]"')
        elif t.lastgroup == 'number':
            out.append('"[redacted]"')
        elif t.group() in ('`', '/', '"', "'"):
            # Templates/regex literals need a full JS grammar. Do not expose
            # an unrecognised literal or partially redact it.
            return '/* [redacted: unsupported JavaScript literal] */'
        else:
            out.append(t.group())
        end = t.end()
    out.append(source[end:])
    return ''.join(out)

class SourceRedactor(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=False)
        self.parts = []
        self.script = False
    def handle_starttag(self, tag, attrs):
        self.parts.append('<'+tag)
        for k,v in attrs:
            if v is None:
                self.parts.append(' '+k)
                continue
            if k in ('name','id','type') and re.fullmatch(r'[\w.$:-]+',v):
                safe = v
            elif k in ('action','src','href') and re.fullmatch(r'/[A-Za-z0-9_./-]+\.(?:cgi|asp|js|css)',v):
                safe = v
            else:
                safe = '[redacted]'
            self.parts.append(' '+k+'="'+html.escape(safe,quote=True)+'"')
        self.parts.append('>')
        self.script = tag == 'script'
    def handle_startendtag(self,tag,attrs):
        self.handle_starttag(tag,attrs)
        self.parts[-1] = '/>'
        self.script = False
    def handle_endtag(self,tag):
        self.parts.append('</'+tag+'>')
        if tag == 'script': self.script = False
    def handle_data(self,data):
        self.parts.append(redact_script(data) if self.script else ('[redacted]' if data.strip() else data))
    def handle_comment(self,data): self.parts.append('<!-- [redacted] -->')
    def handle_entityref(self,name): self.parts.append('[redacted]')
    def handle_charref(self,name): self.parts.append('[redacted]')
    def handle_decl(self,decl): self.parts.append('<!DOCTYPE html>')

def redact_source(source):
    parser = SourceRedactor()
    parser.feed(source)
    parser.close()
    return scrub_text(''.join(parser.parts))
