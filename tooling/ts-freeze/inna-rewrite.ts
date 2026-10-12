// Preloaded into the TypeScript CLI by inna-parity.ts for the contract set: rust/inna-mcp/tests/ts/
// rewrite.ts, which parity.ts preloaded, with the electronic-ID host `innskra.island.is` routed
// too, as the binary built with `test-origin` routes it. Requests for those hosts go to the local
// upstream in INNA_TEST_ORIGIN, as `<origin>/<host><path><query>`, and `Date.now` reads the clock
// file in INNA_TEST_NOW. Cookies and URLs the code sees are unchanged. PR 3 deletes this script
// with the TypeScript sources.
import { readFileSync } from 'node:fs';

const origin = process.env.INNA_TEST_ORIGIN;

if (origin?.startsWith('http://127.0.0.1:') !== true)
  throw new RangeError('INNA_TEST_ORIGIN must be local.');

const clock = process.env.INNA_TEST_NOW;

if (clock !== undefined) Date.now = () => Number(readFileSync(clock, 'utf8'));

const upstream = globalThis.fetch;

const routed = (hostname: string) =>
  hostname === 'inna.is' || hostname.endsWith('.inna.is') || hostname === 'innskra.island.is';

globalThis.fetch = Object.assign(
  (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : input);

    if (url.protocol !== 'https:' || url.port !== '' || !routed(url.hostname))
      throw new RangeError('Unexpected request origin.');

    return upstream(`${origin}/${url.hostname}${url.pathname}${url.search}`, init);
  },
  { preconnect: upstream.preconnect },
);
