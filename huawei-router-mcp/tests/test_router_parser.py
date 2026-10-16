import unittest
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from router_parser import parse_constructors, sanitize, bounded, redact_source

class ParserTests(unittest.TestCase):
    def test_escapes_quotes_and_commas(self):
        html = r'''function USERDevice(Domain,IpAddr,HostName,WifiKey){ }
        var d = new USERDevice("LAN","192.168.1.20","Bob\x2c \"Laptop\"","not-for-output");'''
        result = parse_constructors(html)
        assert result["USERDevice"][0]["HostName"] == 'Bob, "Laptop"'
        assert sanitize(result)["USERDevice"][0] == {
            "Domain":"LAN","IpAddr":"192.168.1.20","HostName":'Bob, "Laptop"'
        }

    def test_multiple_calls(self):
        html = 'function X(A,B){} new X("a,b",true); new X("c",12);'
        assert parse_constructors(html)["X"] == [
            {"A":"a,b","B":True},{"A":"c","B":12}
        ]

    def test_sensitive_keys(self):
        assert sanitize({"password":"x","WifiPSK":"y","csrfToken":"z",
                         "wep_mode":"w","ssid":"ok","nested":[{"APIKEY":"k"}]}) == {
                             "ssid":"ok","nested":[{}]
                         }

    def test_bounded(self):
        assert "of stór" in bounded({"long":"a"*60_000})

    def test_source_redaction(self):
        source = r'''<input name="onttoken" value="hidden1"><div>hidden2</div>
        <!-- hidden3 --><script>var secret='hidden4'; var x="\x68idden5";
        function X(Name,Password){} new X('name','hidden6');
        Form.setAction('/login.cgi'); var csrf=123456;</script>'''
        result = redact_source(source)
        for value in ('hidden1','hidden2','hidden3','hidden4','hidden5','hidden6','123456'):
            self.assertNotIn(value,result)
        self.assertIn('/login.cgi',result)
        self.assertIn('function X(Name,Password)',result)
        self.assertIn('[redacted]',result)

    def test_unsupported_discovery_literals(self):
        for source in ('<script>var token=`hidden`;</script>', '<script>var x=/hidden/;</script>'):
            self.assertNotIn('hidden',redact_source(source))

    def test_escape_decoded_once(self):
        source = r'''function X(A,B,C){} new X("\\x41","\u00e9","\uD83D\uDE00")'''
        self.assertEqual(parse_constructors(source)['X'][0], {'A':r'\x41','B':'é','C':'😀'})

    def test_nonexecuting_and_opaque_strings(self):
        source = '''function X(A,B){} // new X("bad", "bad")
        var text="new X('bad','bad')"; /* new X('bad','bad') */
        new X(dangerous("secret"),[1,2]); new X(0x10,1.2e3);'''
        self.assertEqual(parse_constructors(source)['X'],[{'A':None,'B':None},{'A':16,'B':1200.0}])

    def test_nested_filter_and_quoted_log_value(self):
        value = {'nested':[{'csrf':'x','Message':'password="long secret" other=ok'}]}
        self.assertEqual(sanitize(value),{'nested':[{'Message':'password=[redacted] other=ok'}]})

if __name__ == "__main__":
    unittest.main()
