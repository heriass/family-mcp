"""Exercise prepared HTTP requests and real cookie extraction, without sockets."""
import io
import json
import os
import sys
import unittest
from contextlib import redirect_stderr
from email.message import Message
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from urllib.parse import urlparse, parse_qs

import requests
from requests.adapters import BaseAdapter
from requests.cookies import extract_cookies_to_jar

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import server
from test_http_tools import ENV, HOST, LOGOUT, ORIGINAL_SESSION


class BrowserAdapter(BaseAdapter):
    def __init__(self, *, verify_status=200, verify_body='<html>Router index</html>', page_status=200, fail=None):
        self.sent = []
        self.closed = False
        self.verify_status = verify_status
        self.verify_body = verify_body
        self.page_status = page_status
        self.fail = fail

    def send(self, request, **kwargs):
        self.sent.append(request)
        path = urlparse(request.url).path
        if self.fail and path == self.fail[0]:
            raise self.fail[1]
        status, body, cookies = 200, '', []
        if path == '/':
            body = '<input id="txt_Username">'
            cookies = ['BootSession=bootstrap-private; Path=/; HttpOnly']
        elif path == '/asp/GetRandCount.asp':
            body = 'nonce-private'
            cookies = ['NonceSession=nonce-cookie-private; Path=/']
        elif path == '/login.cgi':
            # A generic 200 login response is not authentication proof.
            body = 'generic-login-response'
            cookies = ['SessionID=auth-private; Path=/; HttpOnly', 'SessionToken=token-private; Path=/']
        elif path == '/index.asp':
            status, body = self.verify_status, self.verify_body
        elif path == server.PAGES['deviceinfo']:
            status, body = self.page_status, 'function Info(ModelName){}new Info("V261a-20")'
        elif path != '/logout.cgi':
            raise AssertionError('Unexpected request')
        result = requests.Response()
        result.status_code = status
        result.url = request.url
        result.request = request
        result._content = body.encode()
        result.encoding = 'utf-8'
        msg = Message()
        for cookie in cookies:
            msg.add_header('Set-Cookie',cookie)
        result.raw = SimpleNamespace(_original_response=SimpleNamespace(msg=msg))
        # Match HTTPAdapter.build_response's response-cookie extraction.
        extract_cookies_to_jar(result.cookies, request, result.raw)
        return result

    def close(self):
        self.closed = True


@patch.dict(os.environ, ENV)
class BrowserFlowTests(unittest.TestCase):
    def diagnose(self, adapter):
        http = requests.Session()
        http.mount('https://', adapter)
        logs = io.StringIO()
        with redirect_stderr(logs), patch.object(server, 'RouterSession',
                side_effect=lambda **kwargs: ORIGINAL_SESSION(http, **kwargs)):
            result = json.loads(server.router_diagnose())
        self.assertTrue(adapter.closed)
        self.assertEqual(len(http.cookies), 0)
        return result, logs.getvalue()

    def test_browser_order_headers_cookie_retention_and_names_only(self):
        adapter = BrowserAdapter()
        result, logs = self.diagnose(adapter)
        self.assertNotIn('error',result)
        self.assertEqual([(r.method, urlparse(r.url).path) for r in adapter.sent], [
            ('GET','/'),('POST','/asp/GetRandCount.asp'),('POST','/login.cgi'),
            ('GET','/index.asp'),('GET',server.PAGES['deviceinfo']),('POST','/logout.cgi')])
        self.assertEqual([s['step'] for s in result['steps']],
                         ['bootstrap','nonce','login','verify','page:deviceinfo','logout'])
        nonce, login, verify, page = adapter.sent[1:5]
        for request in (nonce, login):
            self.assertEqual(request.headers['Origin'],HOST)
            self.assertEqual(request.headers['Referer'],HOST+'/')
            self.assertEqual(request.headers['Content-Type'],'application/x-www-form-urlencoded')
        self.assertEqual(nonce.headers['X-Requested-With'],'XMLHttpRequest')
        self.assertNotIn('X-Requested-With',login.headers)
        self.assertEqual(page.headers['Referer'],HOST+'/index.asp')
        self.assertEqual(verify.headers['Referer'],HOST+'/')
        self.assertIn('BootSession=bootstrap-private',nonce.headers['Cookie'])
        self.assertNotIn('Cookie=body:',nonce.headers['Cookie'])
        self.assertIn('BootSession=bootstrap-private',login.headers['Cookie'])
        self.assertIn('NonceSession=nonce-cookie-private',login.headers['Cookie'])
        self.assertEqual(login.headers['Cookie'].count('Cookie=body:Language:english:id=-1'),1)
        self.assertIn('SessionID=auth-private',verify.headers['Cookie'])
        self.assertIn('SessionToken=token-private',page.headers['Cookie'])
        self.assertEqual(parse_qs(login.body)['x.X_HW_Token'],['nonce-private'])
        self.assertEqual(result['steps'][2]['set_cookie_names'],['SessionID','SessionToken'])
        for line in map(json.loads,logs.splitlines()):
            self.assertNotIn('set_cookie_names',line)
        for secret in ('bootstrap-private','nonce-private','nonce-cookie-private','auth-private','token-private',
                       'test-user','test-password','dGVzdC1wYXNzd29yZA==','generic-login-response'):
            self.assertNotIn(secret,logs+json.dumps(result))

    def test_language_cookie_is_host_only_path_root(self):
        adapter = BrowserAdapter()
        http = requests.Session()
        http.mount('https://',adapter)
        with redirect_stderr(io.StringIO()), server.authenticated(lambda: ORIGINAL_SESSION(http)) as router:
            self.assertTrue(router.logged_in)
            cookie = next(c for c in http.cookies if c.name == 'Cookie')
            self.assertEqual(cookie.domain,'192.168.1.1')
            self.assertFalse(cookie.domain_specified)
            self.assertEqual(cookie.path,'/')

    def test_failed_index_verification_never_reads_status_and_always_logs_out(self):
        for kwargs in ({'verify_body':'<input id="txt_Username">'}, {'verify_status':403},
                       {'verify_status':302}, {'fail':('/index.asp',requests.Timeout('private-error'))}):
            adapter = BrowserAdapter(**kwargs)
            result, logs = self.diagnose(adapter)
            self.assertIn('verify',result['error'])
            self.assertEqual([s['step'] for s in result['steps']],['bootstrap','nonce','login','verify','logout'])
            self.assertEqual(sum(urlparse(r.url).path == '/login.cgi' for r in adapter.sent),1)
            self.assertEqual(urlparse(adapter.sent[-1].url).path,'/logout.cgi')
            self.assertNotIn('private-error',logs+json.dumps(result))

    def test_403_page_still_attempts_logout(self):
        adapter = BrowserAdapter(page_status=403)
        result, _ = self.diagnose(adapter)
        self.assertIn('page:deviceinfo',result['error'])
        self.assertEqual(result['steps'][-2]['status'],403)
        self.assertEqual(result['steps'][-1]['step'],'logout')

    def test_bootstrap_failure_never_submits_credentials(self):
        adapter = BrowserAdapter(fail=('/',requests.exceptions.SSLError('private-error')))
        result, logs = self.diagnose(adapter)
        self.assertEqual([s['step'] for s in result['steps']],['bootstrap'])
        self.assertIn('SSL-villa',result['error'])
        self.assertEqual(len(adapter.sent),1)
        self.assertNotIn('private-error',logs)
