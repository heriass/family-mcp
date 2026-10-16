import unittest
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

    def test_raw_source_fails_closed(self):
        with self.assertRaises(RuntimeError):
            redact_source("<script>var unknownSecret='abc'</script>")

if __name__ == "__main__":
    unittest.main()
