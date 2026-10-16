"""HTTP mocks never connect to the owner's router."""
import asyncio
import base64
import json
import os
import sys
import threading
import time
import unittest
from http.cookies import SimpleCookie
from pathlib import Path
from unittest.mock import Mock, patch

import requests
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import server

HOST = 'https://192.168.1.1'
LOGOUT = '/logout.cgi?RequestFile=html/logout.html'
ENV = {'ROUTER_USERNAME':'test-user','ROUTER_PASSWORD':'test-password','ROUTER_HOST':HOST}

def response(text='', status=200, headers=None):
    result = requests.Response()
    result.status_code = status
    result._content = text.encode()
    result.encoding = 'utf-8'
    result.headers.update(headers or {})
    cookies = SimpleCookie()
    cookies.load((headers or {}).get('Set-Cookie',''))
    for name, cookie in cookies.items():
        result.cookies.set(name, cookie.value)
    return result

def http_mock(*responses, bootstrap=None, verify=None):
    http = Mock()
    http.cookies = requests.cookies.RequestsCookieJar()
    # Navigation responses are separate so failure fixtures address named
    # nonce/login/page/logout steps rather than fragile sequence offsets.
    pending = iter(responses)
    def request(method, url, **kwargs):
        if method == 'GET' and url == HOST+'/':
            result = bootstrap if bootstrap is not None else response()
        elif method == 'GET' and url == HOST+'/index.asp':
            result = verify if verify is not None else response()
        else:
            result = next(pending)
        if isinstance(result, BaseException):
            raise result
        return result
    http.request.side_effect = request
    return http

@patch.dict(os.environ, ENV)
class SessionTests(unittest.TestCase):
    def test_exact_login_protocol_and_cookie(self):
        http = http_mock(response('nonce'),response(status=302,headers={'Location':'/index.asp'}),response('function X(Name){}new X("ok")'),response())
        with server.authenticated(lambda: server.RouterSession(http)) as router:
            self.assertEqual(server.parse_page(router.get('deviceinfo'))['X'][0]['Name'],'ok')
            self.assertEqual(http.cookies.get('Cookie'),'body:Language:english:id=-1')
        calls = http.request.call_args_list
        self.assertEqual([(c.args[0],c.args[1]) for c in calls], [
            ('GET',HOST+'/'),('POST',HOST+'/asp/GetRandCount.asp'),('POST',HOST+'/login.cgi'),
            ('GET',HOST+'/index.asp'),
            ('GET',HOST+server.PAGES['deviceinfo']),('POST',HOST+LOGOUT)])
        self.assertEqual(calls[2].kwargs['data'],{'UserName':'test-user','PassWord':base64.b64encode(b'test-password').decode(),'Language':'english','x.X_HW_Token':'nonce'})
        self.assertEqual(len(http.cookies),0)
        for call in calls:
            self.assertFalse(call.kwargs['allow_redirects'])
            self.assertEqual(call.kwargs['timeout'],10)
        self.assertFalse(http.verify)
        self.assertFalse(http.trust_env)
        http.close.assert_called_once()

    def test_cleanup_after_http_errors(self):
        for failure in (response(status=500), requests.Timeout('test-password'), response('<input id="txt_Username">')):
            with self.subTest(failure=type(failure).__name__):
                http = http_mock(response('nonce'), response(),failure,response())
                with patch.object(server,'RouterSession',lambda: ORIGINAL_SESSION(http)):
                    data = server.read_pages('devices')
                self.assertIn('error',data)
                self.assertNotIn('test-password',json.dumps(data))
                self.assertTrue(http.request.call_args.args[1].endswith(LOGOUT))
                http.close.assert_called_once()

    def test_login_failure_and_timeout_never_retried(self):
        for failure in (response('<script>function LoginSubmit(){}</script>'), requests.Timeout(),response(status=403),response(status=302,headers={'Location':'https://outside.example/'})):
            http = http_mock(response('nonce'),failure,response())
            with self.assertRaises(Exception):
                with server.authenticated(lambda: server.RouterSession(http)):
                    self.fail('Must not read after failed login')
            self.assertEqual(http.request.call_count,4)
            self.assertTrue(http.request.call_args.args[1].endswith(LOGOUT))
            http.close.assert_called_once()

    def test_logout_failure_is_nonfatal_and_closed(self):
        http = http_mock(response('nonce'),response(),response('function X(A){}new X(1)'),requests.Timeout('test-password'))
        with patch.object(server,'RouterSession',lambda: ORIGINAL_SESSION(http)):
            data = server.read_pages('devices')
        self.assertNotIn('error',data)
        self.assertNotIn('test-password',json.dumps(data))
        http.close.assert_called_once()

    def test_missing_credentials_no_network(self):
        http = http_mock()
        with patch.dict(os.environ,{'ROUTER_PASSWORD':''}):
            with self.assertRaises(server.RouterError):
                with server.authenticated(lambda: server.RouterSession(http)): pass
        http.request.assert_not_called()
        http.close.assert_called_once()

    def test_bad_nonce_no_login_or_logout(self):
        http = http_mock(response('<html>login</html>'))
        with self.assertRaises(server.RouterError):
            with server.authenticated(lambda: server.RouterSession(http)): pass
        self.assertEqual(http.request.call_count,2)
        http.close.assert_called_once()

    def test_calls_are_serialized(self):
        events = []
        class Router:
            def login(self): events.append('login')
            def logout(self): events.append('logout')
            def close(self): pass
        def work():
            with server.authenticated(Router):
                time.sleep(.02)
                events.append('read')
        threads = [threading.Thread(target=work) for _ in range(3)]
        for thread in threads: thread.start()
        for thread in threads: thread.join()
        self.assertEqual(events,['login','read','logout']*3)

    def test_host_rejects_credentials_paths_and_plain_http(self):
        for host in ('http://router','https://user:pass@router','https://router/foo','https://router?x=y','https://'):
            with patch.dict(os.environ,{'ROUTER_HOST':host}), self.assertRaises(ValueError): server.router_host()

ORIGINAL_SESSION = server.RouterSession

class ToolTests(unittest.TestCase):
    def test_all_read_tools_logout_with_http_mocks(self):
        for fn in (server.router_status,server.router_devices,server.router_ports,server.router_wifi,server.router_parental,server.router_access_rules,server.router_logs):
            http = http_mock(*([response('nonce'),response()] + [response('function X(Name,Password){}new X("ok","hidden")')]*3 + [response()]))
            # Make the mock return a page for every read and a successful logout.
            def request(method,url,**kwargs):
                if url.endswith('GetRandCount.asp'): return response('nonce')
                if method == 'GET': return response('function X(Name,Password){}new X("ok","hidden")')
                return response()
            http.request.side_effect = request
            with patch.dict(os.environ,ENV), patch.object(server,'RouterSession',lambda: ORIGINAL_SESSION(http)):
                result = json.loads(fn())
            self.assertNotIn('error',result)
            self.assertNotIn('hidden',json.dumps(result))
            self.assertEqual(sum(c.args[1].endswith('/login.cgi') for c in http.request.call_args_list),1)
            self.assertTrue(http.request.call_args.args[1].endswith(LOGOUT))
            http.close.assert_called_once()

    def test_discovery_disabled_and_unknown_page_no_login(self):
        with patch.object(server,'RouterSession') as factory, patch.dict(os.environ,{'ROUTER_DISCOVERY':'0'}):
            self.assertIn('error',json.loads(server.router_dump_page('devices')))
            factory.assert_not_called()
        with patch.object(server,'RouterSession') as factory, patch.dict(os.environ,{'ROUTER_DISCOVERY':'1'}):
            self.assertIn('error',json.loads(server.router_dump_page('../login.cgi')))
            factory.assert_not_called()

    def test_discovery_redaction_and_logout(self):
        http = http_mock(response('nonce'),response(),response('<script>var token="hidden";Form.setAction("/login.cgi")</script>'),response())
        with patch.dict(os.environ,{**ENV,'ROUTER_DISCOVERY':'1'}), patch.object(server,'RouterSession',lambda: ORIGINAL_SESSION(http)):
            result = json.loads(server.router_dump_page('wlan'))
        self.assertIn('/login.cgi',result['source'])
        self.assertNotIn('hidden',result['source'])
        self.assertTrue(http.request.call_args.args[1].endswith(LOGOUT))

    def test_devices_sort_filter_and_fields(self):
        data = {'devices':{'USERDevice':[
            {'HostName':'Offline laptop','DevStatus':'0','IpAddr':'192.168.1.4'},
            {'HostName':'Online Laptop','DevStatus':'1','MacAddr':'aa','Port':'LAN2','Time':'42'},
            {'HostName':'Phone','DevStatus':'1'}]}}
        with patch.object(server,'read_pages',return_value=data):
            result = json.loads(server.router_devices('LAPTOP'))['devices']
        self.assertEqual([d['name'] for d in result],['Online Laptop','Offline laptop'])
        self.assertEqual(result[0]['port'],'LAN2')
        self.assertEqual(result[0]['connected_time'],'42')

    def test_logs_global_limit_and_validation(self):
        with patch.object(server,'read_pages',return_value={'fwlog':{'A':[{'Message':'a'}]*3,'B':[{'Message':'b'}]*3}}):
            result = json.loads(server.router_logs(limit=4))['fwlog']
        self.assertEqual(sum(map(len,result.values())),4)
        with patch.object(server,'read_pages') as read:
            for args in ({'kind':'bad'},{'limit':0},{'limit':-1},{'limit':1001}):
                self.assertIn('error',json.loads(server.router_logs(**args)))
            read.assert_not_called()

    def test_credentials_redacted_even_if_echoed(self):
        with patch.dict(os.environ,ENV):
            result = server.output({'Message':'test-user test-password dGVzdC1wYXNzd29yZA=='})
        self.assertNotIn('test-user',result)
        self.assertNotIn('test-password',result)
        self.assertNotIn('dGVzdC1wYXNzd29yZA==',result)

    def test_addon_context_matches_canonical(self):
        root = Path(server.__file__).parent
        for name in ('server.py','router_parser.py','requirements.txt'):
            self.assertEqual((root/name).read_bytes(),(root/'addon'/name).read_bytes())
        self.assertTrue((root/'addon'/'run.sh').read_bytes().startswith(b'#!/bin/sh\n'))

class MCPTests(unittest.IsolatedAsyncioTestCase):
    async def test_real_stdio_handshake_tools_and_validation(self):
        from mcp import ClientSession, StdioServerParameters
        from mcp.client.stdio import stdio_client
        params = StdioServerParameters(command=sys.executable,args=[str(Path(server.__file__).resolve())],env={'ROUTER_DISCOVERY':'0'})
        async with stdio_client(params) as (read,write):
            async with ClientSession(read,write) as client:
                await client.initialize()
                tools = await client.list_tools()
                self.assertEqual({t.name for t in tools.tools},{'router_status','router_devices','router_ports','router_wifi','router_parental','router_access_rules','router_logs','router_dump_page','router_diagnose'})
                result = await client.call_tool('router_dump_page',{'name':'wlan'})
                self.assertIn('óvirkur',result.content[0].text)
                result = await client.call_tool('router_logs',{'kind':'invalid'})
                self.assertIn('Veldu',result.content[0].text)

class MappingTests(unittest.TestCase):
    def test_status_port_wifi_normalization(self):
        with patch.object(server,'read_pages',return_value={
            'deviceinfo':{'Info':[{'ModelName':'V261a-20','SoftwareVersion':'mock-fw','UpTime':'123'}]},
            'wan':{'WAN':[{'ConnectionStatus':'Connected','ExternalIPAddress':'203.0.113.1','TotalBytesSent':'44'}]},
            'wanlist':{}}):
            result = json.loads(server.router_status())
        self.assertEqual((result['model'],result['firmware'],result['uptime']),('V261a-20','mock-fw','123'))
        self.assertEqual(result['wan'][0]['bytes_sent'],'44')
        with patch.object(server,'read_pages',return_value={'eth':{'ETH':[{'Name':'LAN1','Status':'Up','Speed':'1000','BytesReceived':'99'}]}}):
            result = json.loads(server.router_ports())['ports'][0]
        self.assertEqual((result['port'],result['speed'],result['bytes_received']),('LAN1','1000','99'))
        with patch.object(server,'read_pages',return_value={'wlan':{'WLAN':[{'SSID':'Home','Enable':'1','Band':'5GHz','BeaconType':'11i','PreSharedKey':'hidden'}]},'guest':{}}):
            result = json.loads(server.router_wifi())
        self.assertEqual(result['wifi'][0]['security'],'11i')
        self.assertNotIn('hidden',json.dumps(result))
        self.assertEqual(result['guest'],[])

    def test_output_limit_uses_valid_json(self):
        result = server.output({'Message':'a'*60001})
        self.assertLessEqual(len(result),60000)
        self.assertIn('error',json.loads(result))

    def test_secret_shaped_like_path_is_redacted(self):
        from router_parser import redact_source
        source = '<script>var token="/hidden.cgi"; function X(Password){}new X("/otherhidden.asp");Form.setAction("/okay.cgi");</script>'
        result = redact_source(source)
        self.assertNotIn('hidden',result)
        self.assertIn('/okay.cgi',result)
