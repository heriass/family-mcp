"""Safe metadata only: no test connects to a real router."""
import base64
import io
import json
import os
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

import requests

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import server
from test_http_tools import ENV, HOST, LOGOUT, ORIGINAL_SESSION, http_mock, response


@patch.dict(os.environ, ENV)
class DiagnosticsTests(unittest.TestCase):
    def diagnose(self, http):
        stderr, stdout = io.StringIO(), io.StringIO()
        with redirect_stderr(stderr), redirect_stdout(stdout), patch.object(
            server, 'RouterSession', side_effect=lambda **kwargs: ORIGINAL_SESSION(http, **kwargs)
        ):
            result = json.loads(server.router_diagnose())
        self.assertEqual(stdout.getvalue(), '')
        lines = [json.loads(line) for line in stderr.getvalue().splitlines()]
        # Strict schema: no request/response representations or arbitrary messages.
        for line in lines:
            self.assertLessEqual(set(line), {'step','status','length','location','exception'})
        for step in result['steps']:
            self.assertEqual(set(step), {'step','status','length','set_cookie_names'} if step['step'] == 'login' else {'step','status','length'})
        self.assertEqual(len(lines), http.request.call_count)
        http.close.assert_called_once()
        return result, lines, stderr.getvalue()

    def test_success_metadata_only_and_no_credentials(self):
        secret = 'nonce-private-marker'
        body = '<!-- test-user test-password -->function Info(Name){}new Info("private-body")'
        http = http_mock(response(secret), response(status=302, headers={
            'Location':'https://192.168.1.1/index.asp?nonce='+secret,
            'Set-Cookie':'session=private-cookie'}), response(body), response())
        result, lines, logs = self.diagnose(http)
        self.assertNotIn('error', result)
        self.assertEqual([s['step'] for s in result['steps']], ['bootstrap','nonce','login','verify','page:deviceinfo','logout'])
        self.assertEqual([s['status'] for s in result['steps']], [200,200,302,200,200,200])
        self.assertEqual(result['steps'][4]['length'],len(body.encode()))
        self.assertEqual(lines[2]['location'],'/index.asp')
        for value in (secret,'test-user','test-password','private-cookie','private-body',
                      base64.b64encode(b'test-password').decode(), 'Set-Cookie'):
            self.assertNotIn(value, logs+json.dumps(result))

    def test_location_drops_query_fragment_authority_and_unknown_path(self):
        for location, expected in (
            ('https://user:pass@router/login.asp?token=secret#secret','/login.asp'),
            ('/private-session-secret/login.asp?token=secret','[redacted]'),
            ('/login.asp?x=secret\nforged-line','/login.asp'),
            ('http://[broken','[redacted]'),
        ):
            self.assertEqual(server.location_path(location),expected)

    def test_http_errors_report_step_and_code(self):
        for step in ('nonce','login','page:deviceinfo','logout'):
            responses = [response('nonce'), response(), response('function X(A){}new X(1)'), response()]
            index = ['nonce','login','page:deviceinfo','logout'].index(step)
            responses[index] = response('private-body',status=503)
            if step == 'nonce': responses = responses[:1]
            elif step == 'login': responses = responses[:2]+[response()]
            result, lines, _ = self.diagnose(http_mock(*responses))
            failed = next(line for line in lines if line['step'] == step)
            self.assertEqual(failed['exception'],'HTTPError')
            self.assertEqual(failed['status'],503)
            if step == 'logout':
                self.assertNotIn('error',result)
            else:
                self.assertIn(step,result['error'])
                self.assertIn('503',result['error'])

    def test_timeout_ssl_and_unexpected_failures_each_step(self):
        for exception, label in ((requests.Timeout,'Tími rann út'),
                                 (requests.exceptions.SSLError,'SSL-villa'),
                                 (requests.ConnectionError,'Tengivilla'),
                                 (RuntimeError,'Óvænt villa')):
            for index, step in enumerate(('nonce','login','page:deviceinfo','logout')):
                responses = [response('nonce'),response(),response('function X(A){}new X(1)'),response()]
                responses[index] = exception('private-error test-password')
                if index == 0: responses = responses[:1]
                elif index == 1: responses = responses[:2]+[response()]
                result, lines, logs = self.diagnose(http_mock(*responses))
                self.assertNotIn('private-error', logs+json.dumps(result))
                self.assertNotIn('test-password', logs+json.dumps(result))
                failed = next(line for line in lines if line['step'] == step)
                self.assertEqual(failed['exception'],exception.__name__)
                self.assertIsNone(failed['status'])
                self.assertIsNone(failed['length'])
                if index == 3:
                    self.assertNotIn('error',result)
                else:
                    self.assertIn(label,result['error'])
                    self.assertIn(step,result['error'])

    def test_login_redirect_and_login_page_failures(self):
        for failure, label in (
            (response(status=302,headers={'Location':'/login.asp?token=private-secret'}),'tilvísun'),
            (response(status=302,headers={'Location':'https://other.example/index.asp?secret=x'}),'tilvísun'),
            (response('<input id="txt_Username" value="private-secret">'),'Innskráningarsíða'),
        ):
            http = http_mock(response('nonce'),failure,response())
            result, lines, logs = self.diagnose(http)
            self.assertIn(label,result['error'])
            self.assertEqual([s['step'] for s in result['steps']],['bootstrap','nonce','login','logout'])
            self.assertEqual(lines[2]['exception'],'RouterError')
            self.assertNotIn('private-secret',logs+json.dumps(result))
            self.assertEqual(sum(c.args[1].endswith('/login.cgi') for c in http.request.call_args_list),1)

    def test_login_page_after_authenticated_read(self):
        http = http_mock(response('nonce'),response(),response('<input id="txt_Password">'),response())
        result, lines, _ = self.diagnose(http)
        self.assertIn('Innskráningarsíða',result['error'])
        self.assertIn('page:deviceinfo',result['error'])
        self.assertEqual(lines[4]['exception'],'RouterError')
        self.assertEqual(result['steps'][-1]['step'],'logout')

    def test_parser_valueerror_and_logout_failure_preserve_primary_error(self):
        http = http_mock(response('nonce'),response(),response('private-body'),response(status=500))
        with patch.object(server,'parse_page',side_effect=ValueError('private-parser-secret')):
            result, lines, logs = self.diagnose(http)
        self.assertIn('þátta síðu deviceinfo',result['error'])
        self.assertEqual(lines[4]['exception'],'ValueError')
        self.assertEqual(lines[5]['exception'],'HTTPError')
        self.assertNotIn('private-parser-secret', logs+json.dumps(result))

    def test_regular_tools_also_map_parser_errors(self):
        http = http_mock(response('nonce'),response(),response('function X(A){}new X("unfinished"'),response())
        stderr = io.StringIO()
        with redirect_stderr(stderr), patch.object(server,'RouterSession',return_value=ORIGINAL_SESSION(http)):
            result = server.read_pages('devices')
        lines = [json.loads(line) for line in stderr.getvalue().splitlines()]
        self.assertIn('þátta síðu devices',result['error'])
        self.assertEqual(lines[4]['exception'],'ValueError')
        self.assertEqual(lines[-1]['step'],'logout')

    def test_addon_keeps_reviewed_network_and_boot_settings(self):
        config = (Path(server.__file__).parent/'addon'/'config.yaml').read_text()
        self.assertIn('boot: auto',config)
        self.assertNotIn('\nports:',config)
