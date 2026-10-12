// Freezes what the TypeScript Krónan CLI and server do in every scenario of
// rust/kronan-mcp/tests/ts/parity.ts, and in the contract cases of
// rust/kronan-mcp/tests/ts/integration.test.ts the ledger names `kronan/contract/*`, for
// rust/kronan-mcp/tests/replay.rs. Run once per set from the repository root with Bun 1.4.2 at the
// commit named in the provenance:
//
//   BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 \
//     bun tooling/ts-freeze/kronan-parity.ts rust/kronan-mcp/tests/fixtures/parity
//   BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 \
//     bun tooling/ts-freeze/kronan-parity.ts rust/kronan-mcp/tests/fixtures/contract contract
//
// A contract case becomes a scenario of the same steps: each client method or schema check is the
// tool call the case's drop-in makes, and its scripted fetch is a fake mode of its own (`routes`).
// The scenarios, the fake upstream and its modes, and the store, file and normalization helpers
// are parity.ts's, copied without change between the `parity.ts` markers below. What differs is
// what is kept: parity.ts compared the TypeScript side with the binary; this runs the TypeScript
// side alone and writes, per scenario, every step's input and outcome, each upstream exchange
// (the request and the response the fake served) in order, the stored record and the files left.
// MCP traffic is raw newline-delimited JSON-RPC, and each answer is kept as the line the server
// wrote. The copied code changed only where this workspace's lint requires (marked `lint:`), not in
// behaviour. Every value is synthetic. PR 3 deletes this script with the TypeScript sources.
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';

import {
  SessionStoreError,
  defaultKeyProvider,
  defaultSecretRecordPath,
  withSecretStore,
} from '../../packages/session-store/src/index.ts';
import {
  CHECKOUT_TOTAL,
  LIST_TOKEN,
  NOTE_LINE_TOKEN,
  ORDER_TOKEN,
  UPSTREAM_EXTRA,
  approval,
  checkout,
  fixtureResponse,
  product,
  recipe,
} from '../../rust/kronan-mcp/tests/ts/fixtures.ts';

const [output, set = 'parity'] = process.argv.slice(2);

if (output === undefined || (set !== 'parity' && set !== 'contract'))
  throw new RangeError('Usage: kronan-parity.ts OUTPUT_DIRECTORY [parity|contract]');

if (existsSync(output) && readdirSync(output).length)
  throw new RangeError('The output directory must be empty or absent.');

if (process.env.FAMILY_MCP_STORE_TEST_SEAM !== '1')
  throw new RangeError('Keep FAMILY_MCP_STORE_TEST_SEAM=1; the real store is never used.');

const repo = resolve(import.meta.dir, '../..');

const cli = join(repo, 'packages/kronan-mcp/src/cli.ts');

const rewrite = join(repo, 'rust/kronan-mcp/tests/ts/rewrite.ts');

const scratch = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), 'ts-freeze-kronan-'));

type Seen = { method: string; path: string; headers: Record<string, string>; body: string };

type Exchange = {
  request: Seen;
  response: { status: number; headers: Record<string, string>; body: string };
};

/** The fake's mode, and every exchange it had since the scenario began. */
type Fake = { mode: string; exchanges: Exchange[] };

const fake: Fake = { mode: 'ok', exchanges: [] };

// ---- parity.ts (41d6f98): constants, the fake upstream's answers ----
const TOKEN = 'synthetic-token-0123456789';

/** The default legacy token file under a scratch home, as versions before the store wrote it. */
const LEGACY = {
  file: '.config/kronan-mcp/session.json',
  text: JSON.stringify({ version: 1, token: TOKEN }) + '\n',
};

const MAX_BODY = 4 * 1024 * 1024;

/** The order-attempt record beside the default legacy token file. */
const ATTEMPTS = `${LEGACY.file}.order-attempts.json`;

/** Records as the TypeScript client writes them, with keys and values a reader must keep or drop. */
const attemptsText = JSON.stringify({
  version: 1,
  extra: true,
  attempts: [
    {
      id: '1b4e28ba-2fa1-41d2-883f-0016d3cca427',
      tool: 'complete_checkout',
      checkoutToken: '123e4567-e89b-12d3-a456-426614174006',
      fingerprint: 'f'.repeat(64),
      total: 1489,
      state: 'unknown',
      orderToken: null,
      createdAt: '2026-09-25T10:00:00.000Z',
      updatedAt: '2026-09-25T10:00:01.000Z',
      upstreamOnly: UPSTREAM_EXTRA,
    },
    {
      updatedAt: '2026-09-24T10:00:01.000Z',
      createdAt: '2026-09-24T10:00:00.000Z',
      orderToken: ORDER_TOKEN,
      state: 'accepted',
      total: 1.0,
      fingerprint: 'e'.repeat(64),
      checkoutToken: 'mjólk "checkout"',
      tool: 'reserve_pickup_slot',
      id: 'two',
    },
  ],
});

/** A JSON document of exactly `bytes` bytes: a product whose description pads it out. */
function sized(bytes: number): string {
  const base = JSON.stringify({ ...product, description: '' });

  return JSON.stringify({ ...product, description: 'x'.repeat(bytes - base.length) });
}

/** Upstream values that only exact JavaScript number, key order and string handling reproduce. */
function edge(pathname: string): Response | undefined {
  switch (pathname) {
    case '/api/v1/products/SKU-1/':
      return new Response(
        JSON.stringify({ ...product, nutrition: { b: 1, 10: 'x', 2: null, a: 2.5, energy: 1e21 } })
          .replace('"price":499', '"price":499.0')
          .replace('"discountPercent":10', '"discountPercent":-0')
          .replace('"pricePerKilo":499', '"pricePerKilo":1e-7')
          .replace(
            '"name":"Synthetic milk"',
            '"name":"Mj\\u00f3lk \\ud83d\\ude00 \\u2028 \\"q\\" </script>"',
          )
          .replace('"nutrition":{', '"nutrition":{"__proto__":7,'),
        { headers: { 'content-type': 'application/json' } },
      );
    case '/api/v1/products/SKU-2/':
      // Nested nutrition is outside the documented schema.
      return Response.json({ ...product, nutrition: { energy: { kcal: 1 } } });
    case '/api/v1/products/SKU-3/':
      // A fractional price where the schema has an integer.
      return Response.json({ ...product, qtyInSalesUnit: 0.30000000000000004, price: 1.5 });
    case '/api/v1/products/SKU-4/':
      return Response.json({ ...product, price: 9007199254740992 });
    case '/api/v1/products/SKU-5/':
      // Missing optional fields stay missing; nullish ones keep null.
      return Response.json({
        ...product,
        brand: undefined,
        baseComparisonUnit: null,
        countryOfOrigin: undefined,
      });
    case '/api/v1/products/SKU-6/':
      return new Response(sized(MAX_BODY), { headers: { 'content-type': 'application/json' } });
    case '/api/v1/products/SKU-7/':
      return new Response(sized(MAX_BODY + 1), { headers: { 'content-type': 'application/json' } });
    case '/api/v1/products/SKU-8/':
      return new Response('{"sku":', { headers: { 'content-type': 'application/json' } });
    case '/api/v1/products/SKU-9/':
      return new Response('', { status: 200 });
    case '/api/v1/products/SKU-10/':
      return new Response('<html>not json</html>', { headers: { 'content-type': 'text/html' } });
    case '/api/v1/products/SKU-11/':
      return new Response(null, {
        status: 302,
        headers: { location: `${origin}/api/v1/products/SKU-1/` },
      });
    case '/api/v1/products/SKU-12/':
      return new Response(null, { status: 204 });
    case '/api/v1/products/SKU-13/':
      return new Response(`\ufeff${JSON.stringify(product)}`, {
        headers: { 'content-type': 'application/json' },
      });
    case '/api/v1/products/SKU-14/':
      return new Response(` \n${JSON.stringify(product)}\n\t`, {
        headers: { 'content-type': 'application/json' },
      });
    case '/api/v1/products/SKU-15/':
      return new Response(
        JSON.stringify(product).replace('"price":499', '"price":499,"price":500'),
      );
    case '/api/v1/products/SKU-16/':
      return Response.json(product, { status: 299 });
    case '/api/v1/orders/currently-active/':
      return new Response(null, { status: 404 });
    case '/api/v1/recipes/':
      return Response.json({
        count: 60,
        next: 'https://api.kronan.is/api/v1/recipes/?limit=50&offset=50',
        previous: null,
        results: [recipe, recipe],
      });
    case '/api/v1/recipes/favorites/':
      return Response.json({ count: 2, next: null, previous: null, results: [recipe] });
    case '/api/v1/product-lists/':
      return Response.json({ count: 0, next: undefined, previous: null, results: [] });
    default:
      return undefined;
  }
}

/** The charge-bearing endpoints, which the `send*` modes fail after the gate read the checkout. */
// lint: a Set.
const MONEY_PATHS = new Set([
  '/api/v1/slots/delivery/reserve/',
  '/api/v1/slots/pickup/reserve/',
  '/api/v1/checkout/complete/',
  '/api/v1/checkout/add-to-order/',
]);

/** Failures of the money gate's reads and of the request sent after it. */
function money(pathname: string): Response | undefined {
  const sending = MONEY_PATHS.has(pathname);

  switch (fake.mode) {
    case 'checkout-empty':
      return pathname === '/api/v1/checkout/'
        ? Response.json({ ...checkout, lines: [] })
        : undefined;
    case 'checkout-fractional':
      // 1489.0 is the approved total to JavaScript.
      return pathname === '/api/v1/checkout/'
        ? new Response(
            JSON.stringify(checkout).replace(
              `"total":${CHECKOUT_TOTAL},`,
              `"total":${CHECKOUT_TOTAL}.0,`,
            ),
          )
        : undefined;
    case 'no-active-order':
      return pathname === '/api/v1/orders/currently-active/'
        ? new Response(null, { status: 404 })
        : undefined;
    case 'gate500':
      return pathname === '/api/v1/checkout/'
        ? Response.json({ detail: 'secret detail' }, { status: 500 })
        : undefined;
    case 'send400':
      return sending ? Response.json({ detail: 'secret detail' }, { status: 400 }) : undefined;
    case 'send500':
      return sending ? Response.json({ detail: 'secret detail' }, { status: 500 }) : undefined;
    case 'send-mismatch':
      return sending ? Response.json({ orderToken: 1 }, { status: 201 }) : undefined;
    case 'send-redirect':
      return sending
        ? new Response(null, { status: 307, headers: { location: `${origin}/api/v1/checkout/` } })
        : undefined;
    default:
      return undefined;
  }
}

/** The fake answers every documented path; modes change what it answers. */
function handle(request: Request, body: string): Response {
  const url = new URL(request.url);
  const status = /^status(\d{3})$/.exec(fake.mode);

  if (status)
    return Response.json(
      { detail: 'secret detail', path: url.pathname, body },
      { status: Number(status[1]) },
    );
  const charged = money(url.pathname);

  if (charged) return charged;

  switch (fake.mode) {
    case 'ok':
    case 'checkout-empty':
    case 'checkout-fractional':
    case 'no-active-order':
    case 'gate500':
    case 'send400':
    case 'send500':
    case 'send-mismatch':
    case 'send-redirect':
      return fixtureResponse(url.pathname);
    case 'edge':
      return edge(url.pathname) ?? fixtureResponse(url.pathname);
    case 'mismatch':
      return Response.json({ unexpected: UPSTREAM_EXTRA, count: 'one' });
    case 'array':
      return Response.json([]);
    default:
      return Response.json({ path: url.pathname, body }, { status: 404 });
  }
}
// ---- end of parity.ts ----

/** Bytes as UTF-8 text, a leading byte order mark kept (`text()` drops it); never lossy. */
function exact(bytes: ArrayBuffer): string {
  return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
}

/** The fake's own port in a recorded value, which a replay's fake substitutes with its own. */
function placeholder(text: string): string {
  return text.replaceAll(origin, '<origin>');
}

const server = Bun.serve({
  hostname: '127.0.0.1',
  port: 0,
  async fetch(request) {
    const url = new URL(request.url);

    // Other processes probe loopback ports; only Krónan's API paths are recorded.
    if (!url.pathname.startsWith('/api/v1/')) return new Response(null, { status: 404 });
    const bytes = await request.arrayBuffer();
    // What parity.ts's `request.text()` gave the fake.
    const body = new TextDecoder().decode(bytes);
    const headers: Record<string, string> = {};

    for (const name of ['authorization', 'accept', 'content-type', 'content-length']) {
      const value = request.headers.get(name);

      if (value !== null) headers[name] = value;
    }

    const response = routes(fake.mode, url.pathname) ?? handle(request, body);
    const kept: Record<string, string> = {};

    for (const [name, value] of response.headers) kept[name] = placeholder(value);
    fake.exchanges.push({
      request: {
        method: request.method,
        path: `${url.pathname}${url.search}`,
        headers,
        body: exact(bytes),
      },
      response: {
        status: response.status,
        headers: kept,
        body: placeholder(exact(await response.clone().arrayBuffer())),
      },
    });

    return response;
  },
});

const origin = `http://127.0.0.1:${server.port}`;

// ---- parity.ts (41d6f98): steps, inputs and scenarios ----
type Step =
  | { cli: string[]; stdin?: string }
  | { serve: [string, unknown][]; surface?: boolean }
  | { mode: string }
  | { save: string | null }
  | { file: string; text: string; permissions?: number };

type Scenario = { name: string; env?: Record<string, string>; steps: Step[] };

/** Every read tool with documented arguments, defaults left to the schema. */
const reads: [string, unknown][] = [
  ['auth_status', {}],
  ['search_products', { query: 'milk' }],
  [
    'search_products',
    {
      query: 'mjólk',
      page: 2,
      pageSize: 50,
      sortBy: 'price',
      withDetail: true,
      includePurchaseHistory: true,
    },
  ],
  ['get_product', { sku: 'SKU-1' }],
  ['get_product', { barcode: '12345678' }],
  ['lookup_products', { skus: ['SKU-1', 'SKU-2'] }],
  ['list_categories', {}],
  ['list_category_products', { slug: 'dairy' }],
  ['list_category_products', { slug: 'dairy', page: 2 }],
  ['list_product_tags', {}],
  ['list_products_by_tag', { slug: 'vegan', page: 3 }],
  ['list_products_on_sale', {}],
  ['list_favorite_products', { page: 5 }],
  ['list_orders', {}],
  ['list_orders', { limit: 100, offset: 1_000_000, year: 2025, month: 8, type: 'delivery' }],
  ['get_order', { token: ORDER_TOKEN }],
  ['get_active_order', {}],
  [
    'summarize_order_lines',
    { fromYear: 2025, fromMonth: 1, toYear: 2025, toMonth: 6, skus: ['SKU-1', 'SKU-2'] },
  ],
  ['summarize_order_lines', { nameContains: 'mjólk & co/+?' }],
  ['list_purchase_stats', {}],
  ['list_purchase_stats', { limit: 7, offset: 3, sort: 'most_quantity', includeIgnored: true }],
  ['get_shopping_note', {}],
  ['list_archived_shopping_note_lines', {}],
  ['list_product_lists', {}],
  ['get_product_list', { token: LIST_TOKEN }],
  ['list_recipes', { limit: 1 }],
  ['search_recipes', {}],
  [
    'search_recipes',
    {
      query: 'oats',
      tags: [1, 0],
      ingredientTags: [2],
      cuisineTags: [],
      occasionTags: [3],
      page: 2,
      orderBy: 'cooking_time',
    },
  ],
  ['get_recipe', { slug: 'oat-cakes' }],
  ['list_favorite_recipes', { offset: 1 }],
  ['list_addresses', {}],
  ['get_delivery_slots', { addressId: 11 }],
  ['get_pickup_slots', {}],
  ['get_pickup_slots', { chain: 'pikkolo' }],
  ['get_checkout', {}],
  ['preview_checkout_lines', { lines: [{ sku: 'SKU-1', quantity: 2 }, { sku: 'SKU-2' }] }],
];

/** Invalid arguments: zod's issues, their order and abort rules, and the refinements. */
const invalid: [string, unknown][] = [
  ['auth_status', { x: 1 }],
  ['search_products', {}],
  ['search_products', { query: '' }],
  [
    'search_products',
    { query: 'x'.repeat(65), page: 0, pageSize: 51, sortBy: 'Price', withDetail: 'yes' },
  ],
  ['search_products', { query: 1, page: 1.5, pageSize: '2', extra: true, other: null }],
  ['search_products', { query: 'milk', page: 10001, sortBy: 'x'.repeat(33) }],
  ['search_products', { query: 'milk', page: 9007199254740992 }],
  ['search_products', { query: 'milk', page: -9007199254740992 }],
  ['search_products', { query: 'milk', page: null, sortBy: null }],
  ['search_products', { query: '😀'.repeat(33) }],
  ['search_products', { query: '😀'.repeat(32) }],
  ['get_product', {}],
  ['get_product', { sku: 'SKU-1', barcode: '12345678' }],
  ['get_product', { sku: '.' }],
  ['get_product', { sku: '..' }],
  ['get_product', { sku: '...' }],
  ['get_product', { sku: 'a/b' }],
  ['get_product', { sku: '' }],
  ['get_product', { sku: 'x'.repeat(41), barcode: 'abc' }],
  ['get_product', { barcode: '123' }],
  ['get_product', { barcode: '1'.repeat(21) }],
  ['get_product', { sku: undefined, barcode: undefined }],
  ['get_product', { sku: null }],
  ['lookup_products', { skus: [] }],
  ['lookup_products', { skus: Array(31).fill('A') }],
  ['lookup_products', { skus: ['A', 1, '.', '', 'a b'] }],
  ['lookup_products', { skus: 'SKU-1' }],
  ['list_category_products', { slug: 'mjólk-ð_1.2', page: 1 }],
  ['list_category_products', { slug: '.' }],
  ['list_category_products', { slug: '..', page: 0 }],
  ['list_category_products', { slug: 'a b' }],
  ['list_category_products', { slug: '٣' }],
  ['list_category_products', { slug: 'x'.repeat(129) }],
  ['list_products_by_tag', { page: 2 }],
  ['list_products_on_sale', { page: '1' }],
  ['list_orders', { year: 2025 }],
  ['list_orders', { month: 13 }],
  ['list_orders', { year: 1999, month: 0, type: 'other', limit: 0, offset: -1 }],
  ['list_orders', { limit: 101, offset: 1_000_001 }],
  ['get_order', { token: '' }],
  ['get_order', { token: 'a.b' }],
  ['get_order', { token: 'x'.repeat(65) }],
  ['get_order', {}],
  ['summarize_order_lines', {}],
  ['summarize_order_lines', { fromYear: 2025 }],
  ['summarize_order_lines', { fromYear: 2025, nameContains: 'ab', skus: ['A'] }],
  ['summarize_order_lines', { nameContains: 'a' }],
  ['summarize_order_lines', { nameContains: 'ab', skus: [] }],
  ['summarize_order_lines', { fromYear: 2025, fromMonth: 1, toYear: 2025 }],
  ['summarize_order_lines', { skus: Array(11).fill('A') }],
  ['list_purchase_stats', { sort: 'newest', includeIgnored: 1 }],
  ['list_product_lists', { limit: 1.5 }],
  ['get_product_list', { token: 'ü' }],
  ['list_recipes', { offset: '0' }],
  ['search_recipes', { query: 'x'.repeat(65), tags: [-1, 1.5, 'a'], orderBy: 'best' }],
  ['search_recipes', { tags: Array(21).fill(1), ingredientTags: 'a' }],
  ['get_recipe', { slug: '' }],
  ['list_favorite_recipes', { limit: 101 }],
  ['get_delivery_slots', {}],
  ['get_delivery_slots', { addressId: -1 }],
  ['get_delivery_slots', { addressId: 1.5 }],
  ['get_pickup_slots', { chain: 'bonus' }],
  ['preview_checkout_lines', { lines: [] }],
  [
    'preview_checkout_lines',
    { lines: [{ sku: 'SKU-1', quantity: 0, extra: 1 }, { quantity: 501 }, 'x'] },
  ],
  ['preview_checkout_lines', { lines: Array.from({ length: 101 }, () => ({ sku: 'A' })) }],
  ['preview_checkout_lines', { lines: [{ sku: '.', quantity: 1.5 }] }],
  // Rust: non-object `arguments` are a protocol error in the TypeScript SDK; rmcp words the
  // array case differently and reads null as no arguments (the shared runtime's decision).
  ['no_such_tool', {}],
];

/** Every write tool with documented arguments; the order tools run after their own gate. */
const writes: [string, unknown][] = [
  [
    'add_shopping_note_lines',
    {
      lines: [
        { text: 'Mjólk "1L"' },
        { sku: 'SKU-1', quantity: 0 },
        { quantity: 10_000, text: 'x'.repeat(255) },
      ],
    },
  ],
  ['change_shopping_note_line', { token: NOTE_LINE_TOKEN, text: 'Oat milk' }],
  ['change_shopping_note_line', { quantity: 3, token: NOTE_LINE_TOKEN.toUpperCase() }],
  ['toggle_shopping_note_line_complete', { token: NOTE_LINE_TOKEN }],
  ['delete_shopping_note_line', { token: NOTE_LINE_TOKEN }],
  ['clear_shopping_note', { confirm: true }],
  [
    'set_checkout_lines',
    {
      lines: [{ sku: 'SKU-1' }, { sku: 'SKU-2', quantity: 0, substitution: false }],
      replace: true,
    },
  ],
  [
    'set_checkout_lines',
    { replace: false, lines: [{ substitution: true, quantity: 500, sku: 'SKU-1' }] },
  ],
  ['delete_order_lines', { orderToken: ORDER_TOKEN, lineIds: [1, 2], confirm: true }],
  [
    'lower_order_line_quantities',
    { confirm: true, orderToken: ORDER_TOKEN, lineIds: [0], quantity: 0 },
  ],
  [
    'toggle_order_line_substitution',
    { orderToken: ORDER_TOKEN, lineIds: Array(100).fill(9_007_199_254_740_991) },
  ],
];

/** The money tools in the order a session uses them, with the approval of the checkout fixture. */
const orders: [string, unknown][] = [
  ['reserve_delivery_slot', { slotId: 501, addressId: 11, returnBags: false, ...approval }],
  ['reserve_delivery_slot', { slotId: 501, addressId: 11, returnBags: false, ...approval }],
  ['reserve_pickup_slot', { ...approval, slotId: 0, returnBags: true }],
  ['complete_checkout', { slotId: 501, returnBags: true, ...approval }],
  ['add_checkout_to_order', { ...approval, expectedOrderToken: ORDER_TOKEN }],
  ['complete_checkout', { slotId: 501, addressId: 11, returnBags: true, ...approval }],
];

// lint: `orders[index]` without a non-null assertion.
function order(index: number): [string, unknown] {
  const call = orders[index];

  if (call === undefined) throw new RangeError(`No order call ${index}.`);

  return call;
}

/** Gate refusals: nothing is sent and nothing recorded. */
const refusals: [string, unknown][] = [
  [
    'complete_checkout',
    { slotId: 1, returnBags: true, ...approval, expectedCheckoutToken: 'other-checkout' },
  ],
  [
    'reserve_pickup_slot',
    { slotId: 1, returnBags: true, ...approval, expectedTotal: CHECKOUT_TOTAL - 1 },
  ],
  ['add_checkout_to_order', { ...approval, expectedOrderToken: 'other-order' }],
  [
    'reserve_delivery_slot',
    { slotId: 1, addressId: 1, returnBags: true, ...approval, expectedTotal: 0 },
  ],
];

const writeInvalid: [string, unknown][] = [
  ['add_shopping_note_lines', {}],
  ['add_shopping_note_lines', { lines: [] }],
  ['add_shopping_note_lines', { lines: Array.from({ length: 31 }, () => ({ text: 'a' })) }],
  ['add_shopping_note_lines', { lines: [{ text: 'a', sku: 'B', x: 1 }, { x: 1 }, {}, 'x', null] }],
  [
    'add_shopping_note_lines',
    { lines: [{ quantity: 1.5 }, { sku: 5 }, { text: null }, { text: '', quantity: -1 }] },
  ],
  [
    'add_shopping_note_lines',
    { lines: [{ sku: 'A'.repeat(41) }, { sku: 'A'.repeat(33) }, { sku: '..' }, { sku: 'a b' }] },
  ],
  [
    'add_shopping_note_lines',
    {
      lines: [
        { text: 'x'.repeat(256), quantity: 10_001 },
        { sku: ['a'] },
        { sku: Array(33).fill('a') },
      ],
    },
  ],
  ['change_shopping_note_line', { token: 'x', y: 1 }],
  ['change_shopping_note_line', { token: 5 }],
  ['change_shopping_note_line', {}],
  ['change_shopping_note_line', { token: NOTE_LINE_TOKEN }],
  ['change_shopping_note_line', { token: `${NOTE_LINE_TOKEN}\n`, text: '' }],
  ['change_shopping_note_line', { token: NOTE_LINE_TOKEN.replace('-', ''), quantity: 1.5 }],
  ['change_shopping_note_line', { token: NOTE_LINE_TOKEN.replace('1', 'g'), text: 1 }],
  ['toggle_shopping_note_line_complete', { token: '' }],
  ['delete_shopping_note_line', { token: `{${NOTE_LINE_TOKEN}}`, extra: true }],
  ['clear_shopping_note', {}],
  ['clear_shopping_note', { confirm: false }],
  ['clear_shopping_note', { confirm: 'true', other: 1 }],
  ['set_checkout_lines', { lines: [{ sku: 'SKU-1' }] }],
  ['set_checkout_lines', { replace: 'yes' }],
  [
    'set_checkout_lines',
    {
      lines: [{ sku: 'SKU-1', quantity: 501, substitution: 'no', x: 1 }, { quantity: -1 }, 7],
      replace: true,
    },
  ],
  [
    'set_checkout_lines',
    { lines: Array.from({ length: 101 }, () => ({ sku: 'A' })), replace: null },
  ],
  ['reserve_delivery_slot', {}],
  ['reserve_delivery_slot', { confirm: 'true', expectedTotal: -1 }],
  [
    'reserve_delivery_slot',
    { slotId: 1, addressId: 1, returnBags: true, ...approval, confirm: false },
  ],
  [
    'reserve_delivery_slot',
    {
      slotId: -1,
      addressId: 1.5,
      returnBags: 1,
      ...approval,
      expectedCheckoutToken: 'a b',
      extra: 1,
    },
  ],
  ['reserve_pickup_slot', { slotId: 1, returnBags: true, ...approval, addressId: 1 }],
  [
    'reserve_pickup_slot',
    { slotId: 1, returnBags: true, confirm: true, expectedTotal: 1.5, expectedCheckoutToken: '' },
  ],
  ['complete_checkout', { slotId: 1, returnBags: true, ...approval, addressId: -1 }],
  ['complete_checkout', { slotId: 1, returnBags: true, ...approval, addressId: null }],
  [
    'complete_checkout',
    {
      slotId: 9_007_199_254_740_992,
      returnBags: true,
      ...approval,
      expectedTotal: 9_007_199_254_740_992,
    },
  ],
  ['add_checkout_to_order', { ...approval }],
  ['add_checkout_to_order', { ...approval, confirm: 1, expectedOrderToken: 'x'.repeat(65) }],
  ['delete_order_lines', { orderToken: ORDER_TOKEN, lineIds: [1] }],
  ['delete_order_lines', { orderToken: 'a/b', lineIds: [], confirm: true }],
  [
    'lower_order_line_quantities',
    { orderToken: ORDER_TOKEN, lineIds: [1, -1, 1.5, 'a'], confirm: true, quantity: 10_001 },
  ],
  [
    'lower_order_line_quantities',
    { orderToken: ORDER_TOKEN, lineIds: Array(101).fill(1), confirm: true },
  ],
  ['toggle_order_line_substitution', { orderToken: ORDER_TOKEN, lineIds: [1], confirm: true }],
  ['toggle_order_line_substitution', { lineIds: '1' }],
];

/** Each upstream product the edge mode answers with, by SKU. */
const edges = Array.from({ length: 16 }, (_, index): [string, unknown] => [
  'get_product',
  { sku: `SKU-${index + 1}` },
]);

// lint: a typed constant, not an assertion.
const statusReads: [string, unknown][] = [
  ['get_recipe', { slug: 'oat-cakes' }],
  ['list_recipes', {}],
  ['get_active_order', {}],
  ['search_products', { query: 'milk' }],
];

/** Every failure status, on a tool with a not-found message and on one without. */
const statuses = [400, 401, 403, 404, 409, 429, 500, 502].flatMap((status) => [
  { mode: `status${status}` },
  { serve: statusReads },
]);

const scenarios: Scenario[] = [
  { name: 'surface', steps: [{ serve: [], surface: true }] },
  {
    name: 'cli',
    steps: [
      ['--help'],
      ['-h'],
      ['--version'],
      ['-v'],
      ['-hv'],
      ['-vh'],
      ['--nope'],
      ['--nope=1'],
      ['--help=1'],
      ['--version='],
      ['-x'],
      ['-vx'],
      ['-h=1'],
      ['--='],
      ['---x'],
      ['-h', '--nope'],
      ['-h', '--', 'x'],
      ['auth'],
      ['auth', 'nope'],
      ['auth', 'status', 'x'],
      ['auth', 'migrate', ''],
      ['auth', 'logout', 'x'],
      ['auth', 'set', 'a', 'b'],
      ['serve', 'x'],
      ['x'],
      ['-'],
      ['orders'],
      ['orders', 'clear-attempts', 'x'],
      ['orders', 'nope'],
      ['auth', '--', '--help'],
    ].map((args) => ({ cli: args })),
  },
  { name: 'reads', steps: [LEGACY, { mode: 'ok' }, { serve: reads }] },
  {
    name: 'writes',
    steps: [
      LEGACY,
      { mode: 'ok' },
      { serve: writes },
      { mode: 'mismatch' },
      { serve: writes },
      { mode: 'array' },
      { serve: writes },
    ],
  },
  { name: 'write-inputs', steps: [{ serve: writeInvalid }] },
  {
    name: 'write-statuses',
    steps: [
      LEGACY,
      ...[299, 302, 304, 400, 401, 403, 404, 409, 429, 500, 502].flatMap((status) => [
        { mode: `status${status}` },
        { serve: [...writes, ...refusals.slice(0, 1)] },
      ]),
    ],
  },
  // One approval allows one attempt: accepted and unknown records block, and only the CLI clears.
  {
    name: 'orders',
    steps: [
      LEGACY,
      { mode: 'ok' },
      { serve: orders },
      { cli: ['orders', 'clear-attempts'], stdin: 'n\n' },
      { cli: ['orders', 'clear-attempts'], stdin: 'y\n' },
      { serve: [...refusals, order(0)] },
      { mode: 'checkout-empty' },
      { serve: [order(3)] },
      { mode: 'checkout-fractional' },
      { serve: [order(3)] },
    ],
  },
  // After `auth set` or `auth migrate` no legacy token file exists; the record still sits where it
  // would be, and its directory may not exist yet.
  {
    name: 'orders-store',
    steps: [
      { mode: 'ok' },
      { cli: ['auth', 'set', '-'], stdin: TOKEN },
      { serve: [order(0), order(0)] },
      { cli: ['orders', 'clear-attempts'], stdin: 'y\n' },
    ],
  },
  {
    name: 'orders-migrated',
    steps: [
      LEGACY,
      { mode: 'ok' },
      { cli: ['auth', 'migrate'] },
      { serve: [order(3), order(3)] },
      { cli: ['orders', 'clear-attempts'], stdin: 'n\n' },
    ],
  },
  {
    name: 'order-failures',
    steps: [
      LEGACY,
      ...[
        'gate500',
        'no-active-order',
        'send400',
        'send500',
        'send-mismatch',
        'send-redirect',
        'status401',
      ].flatMap((mode) => [
        { mode },
        { serve: [order(4), order(0)] },
        { cli: ['orders', 'clear-attempts'], stdin: 'y\n' },
      ]),
      { file: ATTEMPTS, text: 'not json' },
      { mode: 'ok' },
      { serve: [order(0)] },
      { file: ATTEMPTS, text: attemptsText },
      { serve: [order(2), order(3)] },
    ],
  },
  // Invalid input is refused before the token is read, so no token is needed.
  { name: 'inputs', steps: [{ serve: invalid }] },
  {
    name: 'edge',
    steps: [
      LEGACY,
      { mode: 'edge' },
      {
        serve: [
          ...edges,
          ['get_active_order', {}],
          ['list_recipes', { limit: 100, offset: 10 }],
          ['list_favorite_recipes', { offset: 1 }],
          ['list_product_lists', {}],
        ],
      },
    ],
  },
  {
    name: 'mismatch',
    steps: [LEGACY, { mode: 'mismatch' }, { serve: reads }, { mode: 'array' }, { serve: reads }],
  },
  { name: 'statuses', steps: [LEGACY, ...statuses] },
  // The encrypted store the TypeScript CLI writes is what the binary reads, and after logout it
  // holds no token; a marker means the legacy file is never read.
  {
    name: 'store',
    steps: [
      LEGACY,
      { save: TOKEN.replace('0', 'x') },
      { mode: 'ok' },
      {
        serve: [
          ['auth_status', {}],
          ['get_order', { token: ORDER_TOKEN }],
        ],
      },
      { save: null },
      { serve: [['auth_status', {}]] },
    ],
  },
  {
    name: 'tokens',
    steps: [
      { mode: 'ok' },
      {
        serve: [
          ['auth_status', {}],
          ['get_product', { sku: 'SKU-1' }],
        ],
      },
      { file: LEGACY.file, text: '{"version":1,"token":"short"}' },
      { serve: [['auth_status', {}]] },
      { file: LEGACY.file, text: '{"version":2,"token":"synthetic-token-0123456789"}' },
      { serve: [['auth_status', {}]] },
      { file: LEGACY.file, text: 'not json' },
      { serve: [['auth_status', {}]] },
      { file: LEGACY.file, text: LEGACY.text, permissions: 0o644 },
      { serve: [['auth_status', {}]] },
      { file: LEGACY.file, text: ' '.repeat(16_385) },
      { serve: [['auth_status', {}]] },
      {
        file: LEGACY.file,
        text: JSON.stringify({ version: 1, token: 'other-synthetic-token-42', extra: 1 }),
      },
      { serve: [['auth_status', {}]] },
    ],
  },
  {
    name: 'auth',
    steps: [
      { mode: 'ok' },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'logout'] },
      { file: 'token.txt', text: `  ${TOKEN} \n` },
      { cli: ['auth', 'set', 'token.txt'] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'set', '-'], stdin: 'other-synthetic-token-42\n' },
      { cli: ['auth', 'set'], stdin: `\u00a0\ufeff${TOKEN}\u2028\u3000` },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'set'], stdin: `\u0085${TOKEN}` },
      { cli: ['auth', 'set'], stdin: '' },
      { cli: ['auth', 'set'], stdin: 'short' },
      { cli: ['auth', 'set'], stdin: 'synthetic\r\ntoken-0123' },
      { cli: ['auth', 'set'], stdin: 'synthetic-tökén-0123' },
      { cli: ['auth', 'set'], stdin: 'x'.repeat(4097) },
      { cli: ['auth', 'set'], stdin: ' '.repeat(16_384 - TOKEN.length) + TOKEN },
      { cli: ['auth', 'set'], stdin: ' '.repeat(16_385 - TOKEN.length) + TOKEN },
      { cli: ['auth', 'set'], stdin: 'ð'.repeat(16_385) },
      { cli: ['auth', 'set'], stdin: 'y'.repeat(20_000) },
      { file: 'shared.txt', text: TOKEN, permissions: 0o644 },
      { cli: ['auth', 'set', 'shared.txt'] },
      { cli: ['auth', 'set', 'absent.txt'] },
      { file: 'huge.txt', text: 'x'.repeat(16_385) },
      { cli: ['auth', 'set', 'huge.txt'] },
      { cli: ['auth', 'set', '.config'] },
      { mode: 'status401' },
      { cli: ['auth', 'set', '-'], stdin: 'rejected-synthetic-token' },
      { cli: ['auth', 'status'] },
      { mode: 'status403' },
      { cli: ['auth', 'status'] },
      { mode: 'ok' },
      { cli: ['auth', 'logout'] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { file: LEGACY.file, text: LEGACY.text },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { file: LEGACY.file, text: LEGACY.text },
      { cli: ['auth', 'migrate'] },
      { file: LEGACY.file, text: LEGACY.text },
      { cli: ['auth', 'logout'] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'set', '-'], stdin: TOKEN },
      { serve: [['auth_status', {}]] },
    ],
  },
  {
    name: 'legacy',
    steps: [
      { mode: 'ok' },
      { file: LEGACY.file, text: LEGACY.text },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'status'] },
      { file: LEGACY.file, text: '{"version":1,"token":"short"}' },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'logout'] },
      { file: LEGACY.file, text: LEGACY.text },
      { cli: ['auth', 'migrate'] },
    ],
  },
  {
    name: 'legacy-invalid',
    steps: [
      { mode: 'ok' },
      { file: LEGACY.file, text: '{"version":1,"token":"short"}' },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'status'] },
      { file: LEGACY.file, text: LEGACY.text, permissions: 0o640 },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'logout'] },
      { cli: ['auth', 'migrate'] },
    ],
  },
  {
    name: 'attempts',
    steps: [
      { cli: ['orders', 'clear-attempts'] },
      { file: ATTEMPTS, text: attemptsText },
      ...['n\n', '\n', '', 'yes please\n', 'N'].map((stdin) => ({
        cli: ['orders', 'clear-attempts'],
        stdin,
      })),
      { cli: ['orders', 'clear-attempts'], stdin: '  Y  \nextra' },
      { cli: ['orders', 'clear-attempts'] },
      { file: ATTEMPTS, text: 'not json' },
      { cli: ['orders', 'clear-attempts'], stdin: 'n\n' },
      { cli: ['orders', 'clear-attempts'], stdin: 'Y\n' },
      { file: ATTEMPTS, text: attemptsText, permissions: 0o644 },
      { cli: ['orders', 'clear-attempts'], stdin: 'y' },
      { file: ATTEMPTS, text: JSON.stringify({ version: 1, attempts: [] }) },
      { cli: ['orders', 'clear-attempts'], stdin: 'y' },
    ],
  },
  {
    name: 'attempts-file',
    env: { KRONAN_TOKEN_FILE: 'nested/../t.json' },
    steps: [
      { file: 't.json.order-attempts.json', text: attemptsText },
      { cli: ['orders', 'clear-attempts'], stdin: 'y\n' },
    ],
  },
  {
    name: 'token-file',
    env: { KRONAN_TOKEN_FILE: 'nested/../token.json' },
    steps: [
      { file: 'token.json', text: LEGACY.text },
      { mode: 'ok' },
      { serve: [['auth_status', {}]] },
    ],
  },
];

// lint: the return type is inferred.
function environment(home: string, extra: Record<string, string> = {}) {
  return {
    PATH: process.env.PATH ?? '/usr/bin:/bin',
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_DATA_HOME: join(home, '.local/share'),
    XDG_STATE_HOME: join(home, '.local/state'),
    XDG_CACHE_HOME: join(home, '.cache'),
    BUN_RUNTIME_TRANSPILER_CACHE_PATH: '0',
    FAMILY_MCP_STORE_TEST_SEAM: '1',
    KRONAN_TEST_ORIGIN: origin,
    ...extra,
  };
}

/** Run `work` with the store options of `home`'s environment. */
async function inHome<T>(
  home: string,
  work: (options: Parameters<typeof withSecretStore>[0]) => Promise<T>,
): Promise<T> {
  const saved = { ...process.env };
  Object.assign(process.env, environment(home));

  try {
    return await work({
      path: defaultSecretRecordPath('kronan-mcp'),
      server: 'kronan-mcp',
      profile: 'default',
      purpose: 'token',
      schema: 1,
      maxBytes: 16_384,
      keys: defaultKeyProvider({ server: 'kronan-mcp', profile: 'default' }),
    });
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved);
  }
}

/** The record the TypeScript CLI saves (`null`: logged out), with the key the store creates. */
async function save(home: string, token: string | null): Promise<void> {
  await inHome(home, (options) =>
    withSecretStore(options, async (store) => {
      if (!(await store.exists())) await store.createKey();
      await store.write(JSON.stringify({ version: 1, token }));
    }),
  );
}

/** Each side's attempt ids and timestamps are its own; within a minute of now they compare equal. */
function fresh(text: string): string {
  return text
    .replace(
      /"id":"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}"/g,
      '"id":"<uuid>"',
    )
    .replace(/\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z/g, (time) =>
      Math.abs(Date.parse(time) - Date.now()) < 60_000 ? '<now>' : time,
    );
}

function write(home: string, step: { file: string; text: string; permissions?: number }): void {
  const path = join(home, step.file);
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  writeFileSync(path, step.text, { mode: step.permissions ?? 0o600 });
  chmodSync(path, step.permissions ?? 0o600);
}
// ---- end of parity.ts ----

/** A contract case's scripted fetch, as a fake mode: `respond:<status>:<body>` or a named one. */
function routes(mode: string, pathname: string): Response | undefined {
  const respond = /^respond:(\d{3}):(token|null|upstream|account)$/.exec(mode);

  if (respond) {
    const status = Number(respond[1]);

    switch (respond[2]) {
      case 'token':
        return new Response(TOKEN, { status });
      case 'upstream':
        return Response.json({ type: 'user', upstreamOnly: TOKEN }, { status });
      case 'account':
        return Response.json({ type: 'user', name: 'Account' }, { status });
      default:
        return new Response(null, { status });
    }
  }

  switch (mode) {
    case 'offset-first':
      return Response.json({
        count: 60,
        next: 'https://api.kronan.is/api/v1/recipes/?limit=50&offset=50',
        previous: null,
        results: [recipe, recipe],
      });
    case 'offset-last':
      return Response.json({ count: 2, next: null, previous: null, results: [recipe] });
    case 'nested':
      return nested(pathname);
    case 'flat':
      return Response.json({ ...product, nutrition: { energy: '100 kcal', fat: 2.5, salt: null } });
    case 'cli-ok':
      return Response.json({ type: 'user', name: 'Test' });
    case 'cli-401':
      return new Response('synthetic-token rejected', { status: 401 });
    default:
      return undefined;
  }
}

const NESTED_MARKER = 'nested-upstream-marker-7c1f';

/** integration.test.ts:2039's fetch. */
function nested(pathname: string): Response {
  if (pathname === '/api/v1/products/SKU-1/')
    return Response.json({ ...product, nutrition: { energy: { marker: NESTED_MARKER } } });

  if (pathname === '/api/v1/products/batch/')
    return Response.json({
      results: [{ ...product, nutrition: { list: [NESTED_MARKER] } }],
      missingSkus: [],
    });

  if (pathname === '/api/v1/categories/_dairy/products/')
    return fixtureResponse('/api/v1/categories/dairy/products/');

  if (pathname === '/api/v1/recipes/-recipe/' || pathname === '/api/v1/recipes/mj%C3%B3lk/')
    return fixtureResponse('/api/v1/recipes/oat-cakes/');

  return new Response(null, { status: 404 });
}

/**
 * Contract steps beyond parity.ts's: a CLI run with its own environment (`<home>` in a value is
 * the scenario's home), a file removed if present, a symbolic link, and a snapshot of every path
 * under the home.
 */
type ContractStep =
  | Step
  | { cli: string[]; stdin?: string; env: Record<string, string> }
  | { remove: string }
  | { link: string; target: string }
  | { snapshot: true };

type Contract = { name: string; env?: Record<string, string>; steps: ContractStep[] };

const unexpected = <T extends object>(input: T) => ({ ...input, unexpected: TOKEN });

/** integration.test.ts:681's cases, in order: each the tool call its client method makes. */
const published: [string, unknown][] = [
  ['auth_status', {}],
  ['search_products', { query: 'milk' }],
  ['get_product', { sku: 'SKU-1' }],
  ['get_product', { barcode: '12345678' }],
  ['lookup_products', { skus: ['SKU-1', 'SKU-2'] }],
  ['list_categories', {}],
  ['list_category_products', { slug: 'dairy', page: 2 }],
  ['list_product_tags', {}],
  ['list_products_by_tag', { slug: 'vegan', page: 3 }],
  ['list_products_on_sale', { page: 4 }],
  ['list_favorite_products', { page: 5 }],
  ['list_orders', { year: 2025, month: 8, type: 'delivery' }],
  ['get_order', { token: ORDER_TOKEN }],
  ['get_active_order', {}],
  [
    'summarize_order_lines',
    { fromYear: 2025, fromMonth: 1, toYear: 2025, toMonth: 6, skus: ['SKU-1', 'SKU-2'] },
  ],
  ['summarize_order_lines', { nameContains: 'milk' }],
  ['list_purchase_stats', { limit: 7, offset: 3, sort: 'most_quantity', includeIgnored: true }],
  ['get_shopping_note', {}],
  ['list_archived_shopping_note_lines', {}],
  ['list_product_lists', {}],
  ['get_product_list', { token: LIST_TOKEN }],
  ['list_recipes', {}],
  ['search_recipes', { query: 'oats', tags: [1] }],
  ['get_recipe', { slug: 'oat-cakes' }],
  ['list_favorite_recipes', {}],
  ['list_addresses', {}],
  ['get_delivery_slots', { addressId: 11 }],
  ['get_pickup_slots', { chain: 'pikkolo' }],
  ['get_checkout', {}],
  ['add_shopping_note_lines', { lines: [{ text: 'Eggs', quantity: 12 }, { sku: 'SKU-1' }] }],
  ['change_shopping_note_line', { token: NOTE_LINE_TOKEN, quantity: 3 }],
  ['toggle_shopping_note_line_complete', { token: NOTE_LINE_TOKEN }],
  ['delete_shopping_note_line', { token: NOTE_LINE_TOKEN }],
  ['clear_shopping_note', { confirm: true }],
  ['preview_checkout_lines', { lines: [{ sku: 'SKU-1', quantity: 2 }, { sku: 'SKU-2' }] }],
  [
    'set_checkout_lines',
    { lines: [{ sku: 'SKU-1', quantity: 2, substitution: false }], replace: true },
  ],
  ['set_checkout_lines', { lines: [{ sku: 'SKU-1' }], replace: false }],
  ['reserve_delivery_slot', { ...approval, slotId: 501, addressId: 11, returnBags: true }],
  ['reserve_pickup_slot', { ...approval, slotId: 601, returnBags: false }],
  ['complete_checkout', { ...approval, slotId: 501, addressId: 11, returnBags: false }],
  ['complete_checkout', { ...approval, slotId: 601, returnBags: true }],
  ['add_checkout_to_order', { ...approval, expectedOrderToken: ORDER_TOKEN }],
  ['delete_order_lines', { orderToken: ORDER_TOKEN, lineIds: [1, 2], confirm: true }],
  [
    'lower_order_line_quantities',
    { orderToken: ORDER_TOKEN, lineIds: [1], quantity: 0, confirm: true },
  ],
  ['toggle_order_line_substitution', { orderToken: ORDER_TOKEN, lineIds: [1] }],
];

/** integration.test.ts:1959's `toolArguments`, in order. */
const roundTrip: [string, unknown][] = [
  ['auth_status', {}],
  ['search_products', { query: 'milk' }],
  ['get_product', { sku: 'SKU-1' }],
  ['lookup_products', { skus: ['SKU-1'] }],
  ['list_categories', {}],
  ['list_category_products', { slug: 'dairy' }],
  ['list_product_tags', {}],
  ['list_products_by_tag', { slug: 'vegan' }],
  ['list_products_on_sale', {}],
  ['list_favorite_products', {}],
  ['list_orders', {}],
  ['get_order', { token: ORDER_TOKEN }],
  ['get_active_order', {}],
  ['summarize_order_lines', { skus: ['SKU-1'] }],
  ['list_purchase_stats', {}],
  ['get_shopping_note', {}],
  ['list_archived_shopping_note_lines', {}],
  ['list_product_lists', {}],
  ['get_product_list', { token: LIST_TOKEN }],
  ['list_recipes', {}],
  ['search_recipes', {}],
  ['get_recipe', { slug: 'oat-cakes' }],
  ['list_favorite_recipes', {}],
  ['list_addresses', {}],
  ['get_delivery_slots', { addressId: 11 }],
  ['get_pickup_slots', {}],
  ['get_checkout', {}],
  ['add_shopping_note_lines', { lines: [{ text: 'Eggs' }] }],
  ['change_shopping_note_line', { token: NOTE_LINE_TOKEN, text: 'Free-range eggs' }],
  ['toggle_shopping_note_line_complete', { token: NOTE_LINE_TOKEN }],
  ['delete_shopping_note_line', { token: NOTE_LINE_TOKEN }],
  ['clear_shopping_note', { confirm: true }],
  ['preview_checkout_lines', { lines: [{ sku: 'SKU-1', quantity: 2 }] }],
  ['set_checkout_lines', { lines: [{ sku: 'SKU-1' }], replace: false }],
  ['reserve_delivery_slot', { ...approval, slotId: 501, addressId: 11, returnBags: false }],
  ['reserve_pickup_slot', { ...approval, slotId: 601, returnBags: false }],
  ['complete_checkout', { ...approval, slotId: 501, addressId: 11, returnBags: false }],
  ['add_checkout_to_order', { ...approval, expectedOrderToken: ORDER_TOKEN }],
  ['delete_order_lines', { orderToken: ORDER_TOKEN, lineIds: [1], confirm: true }],
  [
    'lower_order_line_quantities',
    { orderToken: ORDER_TOKEN, lineIds: [1], quantity: 0, confirm: true },
  ],
  ['toggle_order_line_substitution', { orderToken: ORDER_TOKEN, lineIds: [1] }],
];

const MONEY_TOOLS = new Set([
  'reserve_delivery_slot',
  'reserve_pickup_slot',
  'complete_checkout',
  'add_checkout_to_order',
]);

/**
 * integration.test.ts:1959 on one server, removing the order-attempt record before every call. Only
 * a money call writes that record, so each money call gets a server of its own after a removal and
 * the other calls share one; the first server also lists the tools.
 */
function roundTripSteps(): ContractStep[] {
  let calls: [string, unknown][] = [];
  const steps: ContractStep[] = [{ serve: calls, surface: true }];

  for (const call of roundTrip) {
    if (!MONEY_TOOLS.has(call[0])) {
      calls.push(call);
      continue;
    }

    calls = [];
    steps.push({ remove: ATTEMPTS }, { serve: [call] }, { serve: calls });
  }

  calls.push(['search_products', { query: 'milk', unexpected: TOKEN }]);

  return steps.filter((step) => !('serve' in step) || step.serve.length || step.surface);
}

/** A CLI run with its own legacy token file and configuration directory, as `runCli` gives one. */
function cliIn(name: string, args: string[], stdin?: string): ContractStep {
  const env = { KRONAN_TOKEN_FILE: `<home>/${name}`, XDG_CONFIG_HOME: `<home>/config-${name}` };

  return stdin === undefined ? { cli: args, env } : { cli: args, stdin, env };
}

const contracts: Contract[] = [
  {
    name: 'offset-pages',
    steps: [
      LEGACY,
      { mode: 'offset-first' },
      { serve: [['list_recipes', { limit: 100, offset: 10 }]] },
      { mode: 'offset-last' },
      { serve: [['list_favorite_recipes', { offset: 1 }]] },
    ],
  },
  {
    name: 'oversized-token-input',
    steps: [
      { file: 'huge.json', text: JSON.stringify({ version: 1, token: 'x'.repeat(20_000) }) },
      cliIn('huge.json', ['auth', 'status']),
      cliIn('saved.json', ['auth', 'set', '-'], 'y'.repeat(20_000)),
      { snapshot: true },
    ],
  },
  {
    name: 'published-paths-and-bodies',
    steps: [
      LEGACY,
      { mode: 'ok' },
      ...published.flatMap((call): ContractStep[] => [{ remove: ATTEMPTS }, { serve: [call] }]),
    ],
  },
  {
    name: 'write-inputs',
    steps: [
      {
        serve: [
          ['set_checkout_lines', { lines: [{ sku: 'SKU-1' }] }],
          ['change_shopping_note_line', { token: NOTE_LINE_TOKEN }],
          ['add_shopping_note_lines', { lines: [{ text: 'Eggs', sku: 'SKU-1' }] }],
          ['add_shopping_note_lines', { lines: [{ quantity: 1 }] }],
          ['add_shopping_note_lines', { lines: [] }],
          [
            'add_shopping_note_lines',
            { lines: Array.from({ length: 31 }, () => ({ text: 'Eggs' })) },
          ],
          ['toggle_shopping_note_line_complete', { token: 'not-a-token' }],
          ['clear_shopping_note', {}],
          ['delete_order_lines', { orderToken: ORDER_TOKEN, lineIds: [1], confirm: false }],
          ['lower_order_line_quantities', { orderToken: ORDER_TOKEN, lineIds: [1], quantity: 1 }],
          ['complete_checkout', { ...approval, slotId: 1, returnBags: false, expectedTotal: -1 }],
          ['reserve_pickup_slot', { ...approval, slotId: 1 }],
        ],
      },
    ],
  },
  {
    name: 'safe-errors-and-strict-inputs',
    steps: [
      LEGACY,
      ...(['401', '403', '429'] as const).flatMap((status): ContractStep[] => [
        { mode: `respond:${status}:token` },
        { serve: [['auth_status', {}]] },
      ]),
      { mode: 'respond:404:token' },
      {
        serve: [
          ['get_product', { sku: 'SKU-1' }],
          ['get_order', { token: ORDER_TOKEN }],
          ['list_category_products', { slug: 'dairy' }],
          ['list_products_by_tag', { slug: 'vegan' }],
          ['get_product_list', { token: LIST_TOKEN }],
          ['get_recipe', { slug: 'oat-cakes' }],
        ],
      },
      { mode: 'respond:404:null' },
      { serve: [['get_active_order', {}]] },
      ...(['500:token', '200:token', '200:upstream'] as const).flatMap((answer): ContractStep[] => [
        { mode: `respond:${answer}` },
        { serve: [['auth_status', {}]] },
      ]),
      { mode: 'respond:200:account' },
      {
        serve: [
          ['get_product', {}],
          ['get_product', { sku: 'SKU-1', barcode: '12345678' }],
          ['list_orders', { year: 2025 }],
          ['summarize_order_lines', { fromYear: 2025, nameContains: 'milk' }],
          ['summarize_order_lines', { nameContains: 'milk', skus: ['SKU-1'] }],
          ['summarize_order_lines', {}],
        ],
      },
      {
        serve: [
          ['list_categories', unexpected({})],
          ['search_products', unexpected({ query: 'milk' })],
          ['get_product', unexpected({ sku: 'SKU-1' })],
          ['lookup_products', unexpected({ skus: ['SKU-1'] })],
          ['list_category_products', unexpected({ slug: 'dairy' })],
          ['list_products_by_tag', unexpected({ slug: 'vegan' })],
          ['list_products_on_sale', unexpected({})],
          ['list_orders', unexpected({})],
          ['get_order', unexpected({ token: ORDER_TOKEN })],
          ['summarize_order_lines', unexpected({ nameContains: 'milk' })],
          ['list_purchase_stats', unexpected({})],
          ['list_recipes', unexpected({})],
          ['get_product_list', unexpected({ token: LIST_TOKEN })],
          ['search_recipes', unexpected({})],
          ['get_recipe', unexpected({ slug: 'oat-cakes' })],
          ['add_shopping_note_lines', unexpected({ lines: [{ text: 'Eggs' }] })],
          ['change_shopping_note_line', unexpected({ token: NOTE_LINE_TOKEN, text: 'Eggs' })],
          ['toggle_shopping_note_line_complete', unexpected({ token: NOTE_LINE_TOKEN })],
          ['clear_shopping_note', unexpected({ confirm: true })],
          ['preview_checkout_lines', unexpected({ lines: [{ sku: 'SKU-1' }] })],
          ['set_checkout_lines', unexpected({ lines: [{ sku: 'SKU-1' }], replace: false })],
          [
            'reserve_delivery_slot',
            unexpected({ ...approval, slotId: 1, addressId: 1, returnBags: false }),
          ],
          ['reserve_pickup_slot', unexpected({ ...approval, slotId: 1, returnBags: false })],
          ['complete_checkout', unexpected({ ...approval, slotId: 1, returnBags: false })],
          ['add_checkout_to_order', unexpected({ ...approval, expectedOrderToken: ORDER_TOKEN })],
          [
            'delete_order_lines',
            unexpected({ orderToken: ORDER_TOKEN, lineIds: [1], confirm: true }),
          ],
          [
            'lower_order_line_quantities',
            unexpected({ orderToken: ORDER_TOKEN, lineIds: [1], quantity: 0, confirm: true }),
          ],
          ['toggle_order_line_substitution', unexpected({ orderToken: ORDER_TOKEN, lineIds: [1] })],
        ],
      },
    ],
  },
  { name: 'tool-annotations-and-results', steps: [LEGACY, { mode: 'ok' }, ...roundTripSteps()] },
  {
    name: 'nested-nutrition-and-encoded-slugs',
    steps: [
      LEGACY,
      { mode: 'nested' },
      {
        serve: [
          ['get_product', { sku: 'SKU-1' }],
          ['lookup_products', { skus: ['SKU-1'] }],
        ],
      },
      { mode: 'flat' },
      { serve: [['get_product', { sku: 'SKU-1' }]] },
      { mode: 'nested' },
      {
        serve: [
          ['list_category_products', { slug: '_dairy' }],
          ['get_recipe', { slug: '-recipe' }],
          ['get_recipe', { slug: 'mjólk' }],
          ['list_category_products', { slug: 'a/b' }],
          ['get_recipe', { slug: '../x' }],
          ...['.', '..'].flatMap((segment): [string, unknown][] => [
            ['get_recipe', { slug: segment }],
            ['list_category_products', { slug: segment }],
            ['get_product', { sku: segment }],
            ['lookup_products', { skus: [segment] }],
          ]),
          ['get_product', { sku: 'a.b' }],
        ],
      },
    ],
  },
  {
    name: 'stdio-missing-token',
    env: { KRONAN_TOKEN_FILE: 'missing.json' },
    steps: [{ serve: [['auth_status', {}]] }],
  },
  {
    name: 'cli-offline-commands',
    steps: [
      { file: 'token.txt', text: `  ${TOKEN} \n` },
      { mode: 'cli-ok' },
      cliIn('saved.json', ['auth', 'set', 'token.txt']),
      cliIn('saved.json', ['auth', 'status']),
      cliIn('stdin.json', ['auth', 'set', '-'], `${TOKEN}\n`),
      cliIn('stdin.json', ['auth', 'status']),
      { snapshot: true },
      { mode: 'cli-401' },
      cliIn('rejected.json', ['auth', 'set', 'token.txt']),
      { mode: 'cli-ok' },
      { file: 'shared-token.txt', text: `${TOKEN}\n`, permissions: 0o644 },
      { link: 'linked-token.txt', target: 'token.txt' },
      cliIn('unsafe.json', ['auth', 'set', 'shared-token.txt']),
      cliIn('unsafe.json', ['auth', 'set', 'linked-token.txt']),
      cliIn('unsafe.json', ['auth', 'set', 'absent-token.txt']),
      { snapshot: true },
      { file: 'token.txt', text: 'short\n' },
      cliIn('invalid.json', ['auth', 'set', 'token.txt']),
      cliIn('missing.json', ['auth', 'status']),
      { file: 'saved.json', text: `${JSON.stringify({ version: 1, token: TOKEN })}\n` },
      cliIn('saved.json', ['auth', 'logout']),
      cliIn('saved.json', ['auth', 'status']),
      cliIn('saved.json', ['--version']),
      cliIn('saved.json', ['--help']),
      cliIn('saved.json', ['not-a-command']),
    ],
  },
  {
    // Not a drop-in case: with :1725's 28 unknown-key inputs and the other contract and parity
    // inputs, every one of the 41 tools gets an invalid input whose frozen answer the replay
    // checks (lead 81512). No request may follow any of them.
    name: 'strict-inputs-of-every-other-tool',
    steps: [
      LEGACY,
      {
        serve: [
          ['get_active_order', unexpected({})],
          ['get_checkout', unexpected({})],
          ['get_shopping_note', unexpected({})],
          ['list_addresses', unexpected({})],
          ['list_archived_shopping_note_lines', unexpected({})],
          ['list_favorite_products', unexpected({})],
          ['list_favorite_products', { page: 0 }],
          ['list_product_tags', unexpected({})],
        ],
      },
    ],
  },
];

/**
 * Every path under `home`, sorted, with its permission bits; a directory ends in `/`, a symbolic
 * link names its target instead.
 */
function tree(home: string): string[] {
  return readdirSync(home, { recursive: true, withFileTypes: true })
    .map((entry) => {
      const path = relative(home, join(entry.parentPath, entry.name));

      if (entry.isSymbolicLink()) return `${path} -> ${readlinkSync(join(home, path))}`;
      const mode = (statSync(join(home, path)).mode & 0o777).toString(8);

      return entry.isDirectory() ? `${path}/ ${mode}` : `${path} ${mode}`;
    })
    .toSorted();
}

/** parity.ts `stored`: what the store holds, the record as its decrypted text. */
async function stored(
  home: string,
): Promise<{ store: 'none' } | { record: string | null } | { error: string }> {
  return inHome(home, async (options) => {
    if (!existsSync(options.path) && !existsSync(`${options.path}.marker`))
      return { store: 'none' };

    try {
      return { record: await withSecretStore(options, (store) => store.read()) };
    } catch (error) {
      return { error: error instanceof SessionStoreError ? error.code : 'unknown' };
    }
  });
}

/** parity.ts `files`: `.config/kronan-mcp` sorted, `.json` files with their normalized text. */
function files(home: string): (string | { name: string; text: string })[] {
  const directory = join(home, '.config/kronan-mcp');

  if (!existsSync(directory)) return [];

  return readdirSync(directory)
    .toSorted()
    .map((name) =>
      name.endsWith('.json')
        ? { name, text: fresh(readFileSync(join(directory, name), 'utf8')) }
        : name,
    );
}

function local(home: string, text: string): string {
  return text.replaceAll(realpathSync(home), '<home>').replaceAll(home, '<home>');
}

type CliOutcome = {
  cli: string[];
  stdin?: string;
  code: number;
  stdout: string;
  stderr: string;
  exchanges: Exchange[];
};

async function runCli(home: string, env: Record<string, string>, args: string[], stdin?: string) {
  const start = fake.exchanges.length;

  const child = Bun.spawn([process.execPath, '--preload', rewrite, cli, ...args], {
    cwd: home,
    env: environment(home, env),
    stdin: stdin === undefined ? 'ignore' : new Blob([stdin]),
    stdout: 'pipe',
    stderr: 'pipe',
  });

  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);

  const outcome: CliOutcome = {
    cli: args,
    code,
    stdout: fresh(local(home, stdout)),
    stderr: local(home, stderr),
    exchanges: fake.exchanges.slice(start),
  };

  if (stdin !== undefined) outcome.stdin = stdin;

  return JSON.stringify(outcome);
}

/** The protocol version and client the replay sends too. */
const INITIALIZE = {
  protocolVersion: '2025-11-25',
  capabilities: {},
  clientInfo: { name: 'kronan-parity', version: '1.0.0' },
};

type Params = typeof INITIALIZE | { name: string; arguments: unknown } | { cursor?: string };

/** A request, or the `initialized` notification. */
type Outgoing = { jsonrpc: '2.0'; id?: number; method: string; params?: Params };

/** Newline-delimited lines from `stream`, in order. */
async function* lines(stream: ReadableStream<Uint8Array>): AsyncGenerator<string> {
  const decoder = new TextDecoder();
  let buffer = '';

  for await (const chunk of stream) {
    buffer += decoder.decode(chunk, { stream: true });

    for (let end = buffer.indexOf('\n'); end >= 0; end = buffer.indexOf('\n')) {
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);

      if (line.trim()) yield line;
    }
  }
}

/**
 * `kronan-mcp serve` over raw JSON-RPC, one request at a time. Each answer is kept as the line the
 * server wrote, with any notifications before it, and the exchanges the request caused.
 */
async function runServe(
  home: string,
  env: Record<string, string>,
  calls: [string, unknown][],
  surface = false,
) {
  const child = Bun.spawn([process.execPath, '--preload', rewrite, cli, 'serve'], {
    cwd: home,
    env: environment(home, env),
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  });

  const incoming = lines(child.stdout);
  let id = 0;

  async function send(message: Outgoing): Promise<void> {
    await child.stdin.write(`${JSON.stringify(message)}\n`);
    await child.stdin.flush();
  }

  /** The request's notifications and answer: the next line with an id, which must be this one's. */
  async function request(
    method: string,
    params: Params,
  ): Promise<{ notifications: string[]; answer: string }> {
    const sent = id++;
    const notifications: string[] = [];
    await send({ jsonrpc: '2.0', id: sent, method, params });

    for (;;) {
      const next = await incoming.next();

      if (next.done === true) throw new RangeError(`The server closed before answering ${method}.`);
      const message: unknown = JSON.parse(next.value);

      if (!(message instanceof Object) || !('jsonrpc' in message) || message.jsonrpc !== '2.0')
        throw new RangeError('Not a JSON-RPC 2.0 message.');

      if (!('id' in message)) {
        notifications.push(next.value);
        continue;
      }

      if (message.id !== sent)
        throw new RangeError(`An answer to ${JSON.stringify(message.id)}, not ${sent}.`);

      return { notifications, answer: next.value };
    }
  }

  const parts: string[] = [];
  const initialize = await request('initialize', INITIALIZE);
  await send({ jsonrpc: '2.0', method: 'notifications/initialized' });

  if (surface) {
    const list = await request('tools/list', {});
    parts.push(`"initialize":${initialize.answer}`, `"list":${list.answer}`);
  }

  const results: string[] = [];

  for (const [name, args] of calls) {
    const start = fake.exchanges.length;
    const { notifications, answer } = await request('tools/call', { name, arguments: args });
    const before = notifications.length ? `"notifications":[${notifications.join(',')}],` : '';
    results.push(
      `{"call":${JSON.stringify([name, args])},${before}"answer":${answer},"exchanges":${JSON.stringify(fake.exchanges.slice(start))}}`,
    );
  }

  await child.stdin.end();
  const [stderr, code] = await Promise.all([new Response(child.stderr).text(), child.exited]);
  parts.push(
    `"results":[\n${results.join(',\n')}\n]`,
    `"code":${code}`,
    `"stderr":${JSON.stringify(local(home, stderr))}`,
  );

  return `{"serve":true,${parts.join(',')}}`;
}

/**
 * The scenario as fixture text: its steps one per line (a `serve` step's calls one per line), the
 * number of requests, the stored record and the files left.
 */
async function run(scenario: Contract): Promise<{ text: string; requests: number }> {
  const home = join(scratch, scenario.name);
  mkdirSync(home, { recursive: true, mode: 0o700 });
  fake.mode = 'ok';
  fake.exchanges = [];
  const steps: string[] = [];
  const env = scenario.env ?? {};

  for (const step of scenario.steps) {
    if ('cli' in step && 'env' in step) {
      const own = Object.fromEntries(
        Object.entries(step.env).map(([key, value]) => [key, value.replaceAll('<home>', home)]),
      );

      const outcome = await runCli(home, { ...env, ...own }, step.cli, step.stdin);

      // The step's own environment as written, after its arguments.
      steps.push(`{"env":${JSON.stringify(step.env)},${outcome.slice(1)}`);
    } else if ('cli' in step) steps.push(await runCli(home, env, step.cli, step.stdin));
    else if ('serve' in step) steps.push(await runServe(home, env, step.serve, step.surface));
    else if ('remove' in step) {
      rmSync(join(home, step.remove), { force: true });
      steps.push(JSON.stringify(step));
    } else if ('link' in step) {
      symlinkSync(step.target, join(home, step.link));
      steps.push(JSON.stringify(step));
    } else if ('snapshot' in step) steps.push(JSON.stringify({ snapshot: tree(home) }));
    else if ('mode' in step) {
      fake.mode = step.mode;
      steps.push(JSON.stringify(step));
    } else if ('save' in step) {
      await save(home, step.save);
      steps.push(JSON.stringify(step));
    } else {
      write(home, step);
      steps.push(JSON.stringify(step));
    }
  }

  const head = { id: `kronan/${set}/${scenario.name}`, env: scenario.env ?? {} };
  const tail = { requests: fake.exchanges.length, store: await stored(home), files: files(home) };
  let trailer = JSON.stringify(tail).slice(1, -1);

  // A contract case also checks every path its CLI runs left under the home.
  if (set === 'contract') trailer += `,"tree":${JSON.stringify(tree(home))}`;

  const text = `${JSON.stringify(head).slice(0, -1)},"steps":[\n${steps.join(',\n')}\n],${trailer}}`;

  return { text, requests: fake.exchanges.length };
}

/**
 * Long runs of one character (4 MiB bodies, 16 KiB token files) inside JSON strings become
 * `\u0000<character>*<count>\u0000`; the replay expands them. No frozen value holds a NUL.
 */
function pack(text: string): string {
  if (text.includes('\\u0000')) throw new RangeError('A frozen value holds a NUL.');

  return text.replaceAll(
    /(?<!\\)([^\\"])\1{1023,}/g,
    (sequence: string, unit: string) => `\\u0000${unit}*${sequence.length}\\u0000`,
  );
}

mkdirSync(output, { recursive: true });

try {
  for (const scenario of set === 'parity' ? scenarios : contracts) {
    const { text, requests } = await run(scenario);

    if (text.includes(origin) || text.includes(scratch))
      throw new RangeError(`${scenario.name}: a recorded value names the fake or the scratch.`);
    // The text must stay one JSON document.
    JSON.parse(text);
    writeFileSync(join(output, `${scenario.name}.json`), `${pack(text)}\n`);
    process.stdout.write(
      `${scenario.name}: ${scenario.steps.length} steps, ${requests} requests\n`,
    );
  }
} finally {
  await server.stop(true);
  rmSync(scratch, { recursive: true, force: true });
}
