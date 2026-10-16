import os
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from server import parse_page, sanitize, authenticated, output, RouterSession


class ParserTests(unittest.TestCase):
    def test_escapes_quotes_commas(self):
        source = r"""function USERDevice(Name,IpAddr,HostName,PassWord) {} new USERDevice("A\x2cB","192.168.1.20",'O\'Brien, Laptop',"hidden");"""
        row = parse_page(source)["USERDevice"][0]
        self.assertEqual(row["Name"], "A,B")
        self.assertEqual(row["HostName"], "O'Brien, Laptop")
        self.assertNotIn("PassWord", sanitize(row))

    def test_values(self):
        row = parse_page('function X(A,B,C,D){}new X(1,true,null,"a,b")')["X"][0]
        self.assertEqual(row, {"A": 1, "B": True, "C": None, "D": "a,b"})

    def test_redaction(self):
        self.assertEqual(sanitize({"wifiPsk": "x", "apiKey": "y", "HostName": "ok"}), {"HostName": "ok"})
        self.assertNotIn("hello", output({"password": "hello"}))
        self.assertIn("[redacted]", output({"message": "token=hello"}))

    def test_logout_on_success_and_failure(self):
        events = []
        class MockRouter:
            def login(self): events.append("login")
            def logout(self): events.append("logout")
            def close(self): events.append("close")
        with authenticated(MockRouter):
            events.append("read")
        self.assertEqual(events, ["login", "read", "logout", "close"])
        events.clear()
        with self.assertRaises(ValueError):
            with authenticated(MockRouter):
                raise ValueError("oops")
        self.assertEqual(events, ["login", "logout", "close"])

    def test_failed_login_not_retried(self):
        events = []
        class MockRouter:
            def login(self):
                events.append("login")
                raise RuntimeError("fail")
            def logout(self): events.append("logout")
            def close(self): events.append("close")
        with self.assertRaises(RuntimeError):
            with authenticated(MockRouter):
                pass
        self.assertEqual(events, ["login", "logout", "close"])


if __name__ == "__main__":
    unittest.main()
