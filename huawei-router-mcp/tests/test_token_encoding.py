"""Byte-level regressions for browser token decoding and base64 encoding."""
import base64
import json
import os
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

import requests

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import server
from test_http_tools import ORIGINAL_SESSION


def raw_response(raw):
    response = requests.Response()
    response.status_code = 200
    response._content = raw
    response.encoding = 'iso-8859-1'
    return response


class TokenEncodingTests(unittest.TestCase):
    def test_bom_prefixed_nonce_ignores_incorrect_http_charset(self):
        token = 'Ab09'*16
        response = raw_response(b'\xef\xbb\xbf'+token.encode())
        self.assertEqual(len(response.content),67)
        self.assertTrue(response.text.startswith('ï»¿'))
        self.assertEqual(ORIGINAL_SESSION.token_response(response,'nonce'),token)
        self.assertEqual(ORIGINAL_SESSION.token_response(response,'logout_token'),token)

    def test_optional_bom_and_outer_whitespace(self):
        for raw in (b'Ab123',b' Ab123\r\n',b'\xef\xbb\xbf\r\nAb123\t'):
            self.assertEqual(ORIGINAL_SESSION.token_response(raw_response(raw),'nonce'),'Ab123')

    def test_rejects_non_alphanumeric_empty_and_invalid_utf8(self):
        for raw in (b'',b' \r\n',b'\xef\xbb\xbf',b'<html>',b'abc-def',b'abc_def',
                    b'"abc"',b'ab cd',b'ab\x00cd',b'\xffabc',b'\xef\xbb\xbf\xef\xbb\xbfABC',
                    'á123'.encode(),b'abc\n123',b'A'*4097):
            for step in ('nonce','logout_token'):
                with self.subTest(raw_length=len(raw),step=step), self.assertRaises(server.RouterError) as error:
                    ORIGINAL_SESSION.token_response(raw_response(raw),step)
                self.assertIn(step,str(error.exception))

    def test_password_latin1_matches_browser_char_codes(self):
        for password in ('ASCIIpass','Pássÿ','ÆðÖþ','\u0080\u00ff'):
            expected = base64.b64encode(bytes(ord(c) for c in password)).decode()
            self.assertEqual(ORIGINAL_SESSION.encoded_password(password),expected)

    def test_password_preserves_existing_fallback_outside_latin1(self):
        password = 'a\u0100😀'
        self.assertEqual(ORIGINAL_SESSION.encoded_password(password),base64.b64encode(password.encode('utf-8')).decode())

    def test_latin1_and_utf8_base64_echoes_both_redacted(self):
        password = 'Pássÿ'
        encodings = [base64.b64encode(password.encode(enc)).decode() for enc in ('latin-1','utf-8')]
        with patch.dict(os.environ,{'ROUTER_PASSWORD':password}):
            result = server.output({'Message':' '.join(encodings)})
        self.assertNotIn(password,result)
        for encoding in encodings:
            self.assertNotIn(encoding,result)
        self.assertIn('[redacted]',json.loads(result)['Message'])
