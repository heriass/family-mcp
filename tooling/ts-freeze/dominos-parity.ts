// Freezes what the TypeScript Domino’s CLI and server do in every case of
// rust/dominos-mcp/tests/ts/parity.ts (with reads-parity.ts, auth-parity.ts and money-parity.ts),
// for rust/dominos-mcp/tests/replay.rs. Run once from the repository root with Bun 1.4.2 at the
// commit named in the provenance:
//
//   BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 \
//     bun tooling/ts-freeze/dominos-parity.ts rust/dominos-mcp/tests/fixtures/parity
//
// parity.ts compared the TypeScript side with the binary: the CLI and the server for the argument
// list, the surface and the invalid inputs, and the package's in-process client against the
// binary's drop-in client (one `serve` per tool call) for reads and money. This runs the TypeScript
// side alone, through the same CLI and server the binary replaces, one `serve` per tool call as the
// drop-in client made them. It writes, per case, every step's input and outcome, each upstream
// exchange (the request and the response the fake served) in order, the stored session and every
// path left in the scratch home, with the journal files' text. MCP traffic is raw newline-delimited
// JSON-RPC, and each answer is kept as the line the server wrote. The fixtures, the fake's answers
// and the scenario tables are the parity scripts', copied without change between the markers below;
// the copied code changed only where this workspace's lint requires (marked `lint:`), not in
// behaviour. Every value is synthetic. PR 3 deletes this script with the TypeScript sources.
//
// The `store` set instead freezes what the TypeScript package leaves on disk in the states
// tests/ts/interop.test.ts had the TypeScript side write and the binary continue, for
// rust/dominos-mcp/tests/money.rs and persistence.rs (see that set's PROVENANCE.md).
import { strict as assert } from 'node:assert';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  lstatSync,
  readlinkSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import * as z from 'zod/v4';

import {
  SessionStoreError,
  defaultKeyProvider,
  defaultSecretRecordPath,
  withSecretStore,
  writePrivateFile,
} from '../../packages/session-store/src/index.ts';
import {
  SESSION_MAX_BYTES,
  login as referenceLogin,
  migrate,
  saveSession,
  type Session,
} from '../../packages/dominos-mcp/src/auth.ts';
import { DominosClient as Reference } from '../../packages/dominos-mcp/src/client.ts';
import { parseMenu } from '../../packages/dominos-mcp/src/catalog.ts';
import { cartInput } from '../../packages/dominos-mcp/src/schemas.ts';

const [output, set = 'parity'] = process.argv.slice(2);

if (output === undefined || !['parity', 'contract', 'store'].includes(set))
  throw new RangeError('Usage: dominos-parity.ts OUTPUT_DIRECTORY [parity|contract|store]');

if (existsSync(output) && readdirSync(output).length)
  throw new RangeError('The output directory must be empty or absent.');

if (process.env.FAMILY_MCP_STORE_TEST_SEAM !== '1')
  throw new RangeError('Keep FAMILY_MCP_STORE_TEST_SEAM=1; the real store is never used.');

const repo = resolve(import.meta.dir, '../..');

const cli = join(repo, 'packages/dominos-mcp/src/cli.ts');

// tests/ts/cli.ts restores the arguments Bun would consume (a leading `--`); rewrite.ts routes
// the package's fetch to the fake.
const wrapper = join(repo, 'rust/dominos-mcp/tests/ts/cli.ts');

const rewrite = join(repo, 'rust/dominos-mcp/tests/ts/rewrite.ts');

const scratch = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), 'ts-freeze-dominos-'));

type Fetch = (url: string, init: RequestInit) => Response | Promise<Response>;

type Seen = { method: string; path: string; headers: Record<string, string>; body: string };

type Exchange = {
  request: Seen;
  response: { status: number; headers: Record<string, string>; body: string };
};

// ---- tests/ts/fixtures.ts (41d6f98): the synthetic fixtures and the fake provider ----
const availability = {
  isHidden: false,
  storeAvailability: [],
  availableIn: [],
  notAvailableIn: [],
  availabilityDescription: null,
};

const pizza = {
  id: 'TEST',
  name: 'Synthetic pizza with } and " characters',
  ...availability,
  sizes: [
    { id: 'LG', name: 'Large', pickupPrice: 2500, deliveryPrice: 2500, blockHalfAndHalf: false },
  ],
  crusts: [{ id: 'HANDTOSS', name: 'Classic', allowedSizes: ['LG'] }],
  toppings: [],
  allergens: [],
};

const menu = {
  menuPizzas: [pizza],
  basePizza: pizza,
  sides: [],
  sauces: [],
  beverages: [],
  packages: [],
  allToppings: [],
  allergens: [],
};

const html = `ReactDOM.hydrate(React.createElement(App, ${JSON.stringify({ menu, auth: { access_token: 'DO-NOT-RETURN' } })})); throw new Error('must never execute');`;

const cart = {
  fulfillment: { type: 'pickup' as const, storeId: '1' },
  pizzas: [{ sizeId: 'LG', crustId: 'HANDTOSS', sections: [{ pizzaId: 'TEST' }] }],
};

const profile = {
  id: 1,
  name: 'Test shopper',
  phoneNumber: '3545550123',
  email: null,
  savedAddress: [
    {
      AddressID: 42,
      Address: 'Test street 7',
      PostalCode: '100',
      PostalCodeName: 'Test city',
      AdditionalInfo: 'DO-NOT-RETURN',
    },
  ],
  savedOrders: [],
  access_token: 'DO-NOT-RETURN',
};

const store = {
  RefID: '1',
  Address: 'Test street',
  City: 'Test',
  Zip: '100',
  AcceptInternet: true,
  AcceptsPickup: true,
  AcceptsDelivery: true,
  Disabled: false,
  IsHidden: false,
  Status: 1,
  StoreStatus: 1,
  PickupQuote: '10-15',
  DeliveryQuote: '20-30',
  OpensAt: '11:00',
  ClosesAt: '23:00',
  OpeningHours: '11-23',
  NotificationText: '',
};

class Provider {
  orders = 0;
  payments = 0;
  refreshes = 0;
  minorAmount = 250000;
  losePaymentResponse = false;
  loseOrderResponse = false;
  acceptCheckout = true;
  requireVerification = false;
  latestPaid = false;
  readonly requests: { url: URL; options: RequestInit }[] = [];

  request = async (url: string, options: RequestInit): Promise<Response> => {
    const target = new URL(url);
    this.requests.push({ url: target, options });

    if (target.hostname === 'www.dominos.is') return new Response(html);

    if (target.hostname === 'checkoutshopper-live.adyen.com') return this.payment(target, options);

    assert.equal(target.origin, 'https://api.dominos.is');

    if (target.pathname === '/api/token') {
      this.refreshes++;

      return Response.json({
        access_token: 'rotated-access',
        refresh_token: 'rotated-refresh',
        token_type: 'bearer',
        username: '3545550123',
        expires_in: 3600,
      });
    }

    if (target.pathname === '/api/store') return Response.json([store]);
    assert.match(
      new Headers(options.headers).get('authorization') ?? '',
      /^bearer (synthetic-access|rotated-access)$/,
    );

    if (target.pathname === '/api/user/newuser') return Response.json(profile);

    if (target.pathname === '/api/orders') {
      assert.equal(options.method, 'POST');

      const body = z
        .object({
          Pizzas: z.array(z.object({ Sections: z.array(z.object({ PizzaRefID: z.string() })) })),
          User: z.object({ Username: z.string() }),
          IsFinal: z.boolean(),
          PayWithStraumur: z.boolean().optional(),
          Payonline: z.boolean().optional(),
        })
        .parse(JSON.parse(z.string().parse(options.body)));

      assert.equal(body.Pizzas[0]?.Sections[0]?.PizzaRefID, 'TEST');
      assert.equal(body.User.Username, '3545550123');

      if (!body.IsFinal) return Response.json({ Total: 2500, Success: true });
      assert.equal(body.PayWithStraumur, true);
      assert.equal(body.Payonline, false);
      this.orders++;

      if (this.loseOrderResponse) throw new Error('lost order response');

      return Response.json({
        Total: 2500,
        Success: this.acceptCheckout,
        OrderID: 123,
        OrderGuidId: 'synthetic-order-guid',
        AdyenSessionId: 'synthetic-session',
        AdyenSessionData: 'private-initial-session',
        AdyenClientId: 'private-client-key',
      });
    }

    if (target.pathname === '/api/orders/cart/latest')
      return Response.json(this.latestPaid ? { OrderData: { IsPayed: true } } : { OrderData: '' });

    throw new Error('Unexpected offline fixture request.');
  };

  private payment(url: URL, options: RequestInit): Response {
    assert.equal(new Headers(options.headers).get('authorization'), null);
    assert.equal(url.searchParams.get('clientKey'), 'private-client-key');
    const body: unknown = JSON.parse(z.string().parse(options.body));

    if (url.pathname.endsWith('/setup')) {
      assert.deepEqual(body, { sessionData: 'private-initial-session' });

      return Response.json({
        id: 'synthetic-session',
        sessionData: 'private-rotated-session',
        expiresAt: new Date(Date.now() + 600000).toISOString(),
        amount: { currency: 'ISK', value: this.minorAmount },
        paymentMethods: {
          storedPaymentMethods: [
            {
              id: 'private-vault-token',
              type: 'scheme',
              brand: 'visa',
              lastFour: '1234',
              expiryMonth: '12',
              expiryYear: '2030',
            },
          ],
        },
      });
    }

    assert.ok(url.pathname.endsWith('/payments'));
    assert.deepEqual(body, {
      sessionData: 'private-rotated-session',
      paymentMethod: { type: 'scheme', storedPaymentMethodId: 'private-vault-token' },
      storePaymentMethod: false,
    });
    this.payments++;

    if (this.losePaymentResponse) throw new Error('secret response lost after possible charge');

    if (this.requireVerification)
      return Response.json({
        resultCode: 'ChallengeShopper',
        sessionData: 'private-challenge-session',
        action: { type: 'threeDS2', token: 'private-challenge-token' },
      });

    return Response.json({
      resultCode: 'Authorised',
      sessionData: 'private-final-session',
      sessionResult: 'private-result',
    });
  }
}

const saved = (expired = false, prefix = 'synthetic'): Session => ({
  version: 1,
  accessToken: `${prefix}-access`,
  refreshToken: `${prefix}-refresh`,
  username: '3545550123',
  expiresAt: Date.now() + (expired ? -1 : 3600000),
});
// ---- end of tests/ts/fixtures.ts ----

// ---- tests/ts/reads-parity.ts (41d6f98): the read answers and the failure modes ----
// lint: a named function, not an inline closure over `providers[index]`.
function reads(provider: Provider): Fetch {
  return async (url, options) => {
    const parsed = new URL(url);

    if (parsed.pathname === '/api/addresses')
      return Response.json([
        {
          ID: 42,
          Name: 'Test street 7',
          PostalCode: '100',
          PostalCodeName: 'Test city',
          secret: 'DO-NOT-RETURN',
        },
      ]);

    if (parsed.pathname.endsWith('/GetAddressStoreWithWaitingTimes'))
      return Response.json({ RefID: '1', WaitingTime: '20', secret: 'DO-NOT-RETURN' });

    if (parsed.pathname === '/api/user/getreceipts')
      return Response.json([
        { Id: 'receipt', Amount: 2500, DateOf: '2026-10-10', secret: 'DO-NOT-RETURN' },
      ]);

    if (parsed.pathname === '/api/tracker')
      return Response.json({
        OrderID: 123,
        OrderState: 'Baking',
        Remaining: 10,
        IsPickup: true,
        IsTimedOrder: false,
        secret: 'DO-NOT-RETURN',
      });

    return provider.request(url, options);
  };
}

// lint: the drop-in client's methods as the tool calls they made.
const operations: [string, object][] = [
  ['auth_status', {}],
  ['get_profile', {}],
  ['list_stores', {}],
  ['search_menu', {}],
  ['search_menu', { query: 'SYNTHETIC', kind: 'pizza', offset: 0, limit: 1 }],
  ['search_menu', { query: 'missing', offset: 100 }],
  ['get_menu_item', { kind: 'pizza', id: 'TEST' }],
  ['search_addresses', { query: 'Test & ð' }],
  ['get_delivery_store', { address: 'Test street & ð', postalCode: '100' }],
  ['list_receipts', {}],
  ['get_tracker', {}],
];

const failures = [
  'http',
  'not-modified',
  'refresh-not-modified',
  'transport',
  'json',
  'schema',
  'redirect',
  'large',
  'menu-missing',
  'menu-incomplete',
  'menu-invalid',
];

// lint: a named function of the mode.
function failure(mode: string): Fetch {
  const refresh = mode === 'refresh-not-modified';

  return async (url) => {
    if (refresh) {
      const path = new URL(url).pathname;
      assert.equal(path, '/api/token');

      return new Response(null, { status: 304 });
    }

    if (mode === 'not-modified') return new Response(null, { status: 304 });

    if (mode === 'http') return new Response('secret-error-body', { status: 429 });

    if (mode === 'transport') throw new Error('secret-transport-cause');

    if (mode === 'json') return new Response('secret-invalid-json');

    if (mode === 'schema') return Response.json({ secret: 'secret-invalid-schema' });

    if (mode === 'redirect')
      return new Response('', {
        status: 302,
        headers: { location: 'https://example.invalid/secret' },
      });

    if (mode === 'large') return new Response('x'.repeat(8 * 1024 * 1024 + 1));

    if (mode === 'menu-missing') return new Response('malicious()');

    if (mode === 'menu-incomplete') return new Response('ReactDOM.hydrate({');

    return new Response('ReactDOM.hydrate({menu:malicious()})');
  };
}
// ---- end of tests/ts/reads-parity.ts ----

// ---- tests/ts/auth-parity.ts (41d6f98): the login inputs and the fake's modes ----
const logins: [string, string][] = [
  ['5550123\n123456\n', 'ok'],
  ['+354 555-0123\n 123456 \n', 'ok'],
  ['bad\n123456\n', 'ok'],
  ['', 'ok'],
  ['5550123\n', 'ok'],
  ['5550123\n12345\n', 'ok'],
  ['5550123\n123456\n', '429'],
  ['5550123\n123456\n', '401'],
  ['5550123\n123456\n', 'invalid-token'],
  ['5550123\n123456\n', 'invalid-profile'],
  ['5550123\n123456\n', 'transport'],
];

// lint: a named function of the mode.
function login(mode: string): Fetch {
  return async (url) => {
    const parsed = new URL(url);

    if (parsed.pathname.endsWith('/sendPin')) return new Response('');

    if (mode === 'transport') throw new Error('synthetic-secret-cause');

    if (parsed.pathname.endsWith('/token')) {
      if (mode === '429' || mode === '401')
        return new Response('synthetic-secret-body', { status: Number(mode) });

      if (mode === 'invalid-token')
        return Response.json({ access_token: 'synthetic-secret-value' });

      return Response.json({
        access_token: 'parity-access',
        refresh_token: 'parity-refresh',
        expires_in: 3600,
        username: '3545550123',
        token_type: 'bearer',
      });
    }

    return Response.json(
      mode === 'invalid-profile' ? { secret: 'synthetic-secret-value' } : profile,
    );
  };
}
// ---- end of tests/ts/auth-parity.ts ----

// ---- tests/ts/money-parity.ts (41d6f98): records, scenarios and the fake's answers ----
const Q = '10000000-0000-4000-8000-000000000001';

const C = '10000000-0000-4000-8000-000000000002';

const FUTURE = Date.parse('2100-01-01T00:00:00Z');

const cleanCart = cartInput.parse(cart);

const card = {
  id: 'private-vault-token',
  type: 'scheme',
  brand: 'visa',
  lastFour: '1234',
  expiryMonth: '12',
  expiryYear: '2030',
};

const quote = {
  id: Q,
  username: '3545550123',
  cart: cleanCart,
  total: 2500,
  createdAt: 0,
  expiresAt: FUTURE,
};

const checkout = {
  id: C,
  quoteId: Q,
  username: '3545550123',
  cart: cleanCart,
  total: 2500,
  state: 'ready',
  expiresAt: FUTURE,
  orderId: 123,
  orderGuid: 'private-guid',
  sessionId: 'private-session',
  sessionData: 'private-session-data',
  clientKey: 'private-client-key',
  cards: [card],
};

// lint: the patch values' own type.
type Scenario = {
  name: string;
  tool: 'quote' | 'create' | 'get' | 'pay';
  patch?: Record<string, string | number>;
  mode?: string;
  total?: number;
  cardId?: string;
  cart?: object;
};

const scenarios: Scenario[] = [
  { name: 'quote pickup', tool: 'quote' },
  {
    name: 'quote delivery',
    tool: 'quote',
    cart: {
      ...cart,
      fulfillment: {
        type: 'delivery',
        address: { ID: 42, Name: 'Test & ð', PostalCode: '100', PostalCodeName: 'Test' },
        instructions: 'Door 2',
      },
      coupon: 'SYNTHETIC',
    },
  },
  {
    name: 'quote menu selection refusal',
    tool: 'quote',
    cart: { ...cart, pizzas: [{ ...cart.pizzas[0], sections: [{ pizzaId: 'UNKNOWN' }] }] },
  },
  ...['quote-zero', 'quote-fraction', 'quote-401', 'store-disabled'].map((mode) => ({
    name: mode,
    tool: 'quote' as const,
    mode,
  })),
  { name: 'create ready', tool: 'create' },
  { name: 'quote account changed', tool: 'create', patch: { username: 'other' } },
  { name: 'quote confirmed total changed', tool: 'create', total: 2501 },
  { name: 'quote expired', tool: 'create', patch: { expiresAt: 0 } },
  {
    name: 'quote already used despite expiry',
    tool: 'create',
    patch: { expiresAt: 0, checkoutId: C },
  },
  {
    name: 'quote linked checkout missing',
    tool: 'create',
    patch: { checkoutId: '10000000-0000-4000-8000-000000000003' },
  },
  ...[
    'order-lost',
    'order-rejected',
    'order-schema',
    'order-total',
    'setup-amount',
    'setup-currency',
    'setup-id',
    'setup-date',
    'setup-schema',
  ].map((mode) => ({ name: mode, tool: 'create' as const, mode })),
  { name: 'payment confirmed total changed', tool: 'pay', total: 2501 },
  { name: 'payment account changed', tool: 'pay', patch: { username: 'other' } },
  { name: 'payment expired', tool: 'pay', patch: { expiresAt: 0 } },
  { name: 'payment card missing', tool: 'pay', cardId: 'card_0' },
  { name: 'payment card numeric overflow', tool: 'pay', cardId: `card_${'9'.repeat(400)}` },
  { name: 'payment empty session refuses without send', tool: 'pay', patch: { sessionData: '' } },
  ...[
    'creating',
    'submitting',
    'authorised',
    'refused',
    'pending',
    'requires_action',
    'unknown',
    'amount_changed',
  ].map((state) => ({ name: `payment already ${state}`, tool: 'pay' as const, patch: { state } })),
  ...[
    'Authorised',
    'Refused',
    'Cancelled',
    'Error',
    'Pending',
    'action',
    'payment-schema',
    'payment-401',
    'payment-lost',
  ].map((mode) => ({ name: mode, tool: 'pay' as const, mode })),
  { name: 'get different account', tool: 'get', patch: { username: 'other' } },
  { name: 'get unknown unpaid', tool: 'get', patch: { state: 'unknown' } },
  { name: 'get unknown paid', tool: 'get', patch: { state: 'unknown' }, mode: 'latest-paid' },
  {
    name: 'get nonempty string invalid latest',
    tool: 'get',
    patch: { state: 'unknown' },
    mode: 'latest-schema',
  },
  {
    name: 'get empty string latest',
    tool: 'get',
    patch: { state: 'unknown' },
    mode: 'latest-empty',
  },
  { name: 'get ready never reconciles', tool: 'get', mode: 'latest-schema' },
  { name: 'local record invalid schema', tool: 'get', patch: { total: 1.5 } },
  { name: 'local record invalid json', tool: 'get', mode: 'record-json' },
  { name: 'local record oversized', tool: 'get', mode: 'record-large' },
];

// lint: the per-scenario fetch as a named function of the scenario and its fixture's provider.
function money(scenario: Scenario, provider: Provider): Fetch {
  return async (url, options) => {
    const target = new URL(url);
    const authorization = new Headers(options.headers).get('authorization');

    if (target.hostname === 'checkoutshopper-live.adyen.com') assert.equal(authorization, null);

    if (target.pathname.endsWith('/GetAddressStoreWithWaitingTimes'))
      return Response.json({ RefID: '1', WaitingTime: null });

    if (scenario.mode === 'store-disabled' && target.pathname === '/api/store')
      return Response.json([{ ...store, Disabled: true }]);

    if (target.pathname === '/api/orders') {
      if (scenario.mode === 'quote-401') return new Response('secret-body', { status: 401 });

      if (scenario.mode === 'quote-zero') return Response.json({ Total: 0 });

      if (scenario.mode === 'quote-fraction') return Response.json({ Total: 1.5 });

      if (scenario.mode === 'order-lost') throw new Error('secret-transport');

      if (scenario.mode === 'order-schema')
        return Response.json({ Total: 2500, AdyenSessionId: 'secret-bad/id' });
    }

    if (target.pathname.endsWith('/payments')) {
      if (scenario.mode === 'payment-lost') throw new Error('secret-payment');

      if (scenario.mode === 'payment-401') return new Response('secret-payment', { status: 401 });

      if (scenario.mode === 'payment-schema') return Response.json({ secret: 'secret-payment' });

      if (scenario.mode === 'action')
        return Response.json({
          sessionData: 'private-next',
          resultCode: 'ChallengeShopper',
          action: { type: 'redirect', url: 'https://example.invalid/private-action' },
        });

      if (scenario.mode)
        return Response.json({ sessionData: 'private-next', resultCode: scenario.mode });
    }

    if (target.pathname.endsWith('/cart/latest')) {
      if (scenario.mode === 'latest-paid')
        return Response.json({ OrderData: { IsPayed: true, secret: 'secret-order' } });

      if (scenario.mode === 'latest-schema') return Response.json({ OrderData: 'secret-order' });

      if (scenario.mode === 'latest-empty') return Response.json({ OrderData: '' });
    }

    const response = await provider.request(url, options);

    if (target.pathname === '/api/orders' && scenario.mode === 'order-rejected')
      return Response.json({ ...(await response.json()), Success: false });

    if (target.pathname === '/api/orders' && scenario.mode === 'order-total')
      return Response.json({ ...(await response.json()), Total: 2501 });

    if (target.pathname.endsWith('/setup')) {
      // lint: a parsed value, not `any`.
      const value = z
        .object({
          expiresAt: z.string(),
          id: z.string(),
          amount: z.object({ currency: z.string(), value: z.number() }),
          paymentMethods: z.object({
            storedPaymentMethods: z.array(z.object({ lastFour: z.string() }).loose()),
          }),
        })
        .loose()
        .parse(await response.json());

      value.expiresAt = '2100-01-01T00:00:00Z';

      if (scenario.mode === 'setup-amount') value.amount.value = 2500;

      if (scenario.mode === 'setup-currency') value.amount.currency = 'EUR';

      if (scenario.mode === 'setup-id') value.id = 'private-other';

      if (scenario.mode === 'setup-date') value.expiresAt = '2100-02-29T00:00:00Z';
      const [first] = value.paymentMethods.storedPaymentMethods;

      if (scenario.mode === 'setup-schema' && first) first.lastFour = 'secret';

      return Response.json(value);
    }

    return response;
  };
}

/** The tool call the drop-in client made for `scenario`. */
function moneyCall(scenario: Scenario): [string, object] {
  switch (scenario.tool) {
    case 'quote':
      return ['quote_order', scenario.cart ?? cart];
    case 'create':
      return ['create_checkout', { quoteId: Q, expectedTotal: scenario.total ?? 2500 }];
    case 'get':
      return ['get_checkout', { checkoutId: C }];
    default:
      return [
        'pay_saved_card',
        {
          checkoutId: C,
          cardId: scenario.cardId ?? 'card_1',
          expectedTotal: scenario.total ?? 2500,
          confirm: true,
        },
      ];
  }
}
// ---- end of tests/ts/money-parity.ts ----

// ---- tests/ts/parity.ts (41d6f98): the argument list and the invalid inputs ----
const argumentLists = [
  ['--help'],
  ['--version'],
  ['-hv'],
  ['-vh'],
  ['--help', 'bad'],
  ['--nope'],
  ['--help=1'],
  ['--version='],
  ['-x'],
  ['-é'],
  ['-😀'],
  ['-vx'],
  ['-h=1'],
  ['--='],
  ['---x'],
  ['--no-help'],
  ['--', '--help'],
  ['bad'],
  ['auth'],
  ['auth', 'bad'],
  ['auth', 'status', 'extra'],
  ['serve', 'extra'],
];

// lint: each input as an object.
const invalid: [string, object][] = [
  ['auth_status', { secret: true }],
  ['get_profile', { 0: 1, extra: 2 }],
  ['list_stores', { query: 'x' }],
  ['search_menu', { query: null }],
  ['search_menu', { query: 1 }],
  ['search_menu', { query: '😀'.repeat(101) }],
  ['search_menu', { kind: 'drinks' }],
  ['search_menu', { kind: 1 }],
  ['search_menu', { offset: -1 }],
  ['search_menu', { offset: 1.5 }],
  ['search_menu', { offset: 1e30 }],
  ['search_menu', { limit: 0 }],
  ['search_menu', { limit: 51 }],
  ['search_menu', { limit: 1.5 }],
  ['search_menu', { query: [], limit: false, extra: 1 }],
  ['get_menu_item', {}],
  ['get_menu_item', { kind: 'pizza', id: 'bad/id' }],
  ['get_menu_item', { kind: 'pizza', id: 'a'.repeat(81) }],
  ['get_menu_item', { kind: 'pizza', id: 'TEST\n' }],
  ['search_addresses', { query: 'x' }],
  ['search_addresses', { query: null }],
  ['get_delivery_store', {}],
  ['get_delivery_store', { address: '', postalCode: '1234' }],
  ['get_delivery_store', { address: 'x', postalCode: '100\n' }],
  ['get_delivery_store', { address: [], postalCode: 1, extra: true }],
  ['list_receipts', { card: 1 }],
  ['get_tracker', { extra: 1 }],
  ['quote_order', {}],
  ['quote_order', { fulfillment: { type: 'pickup', storeId: '1' } }],
  ['quote_order', { fulfillment: { type: 'bad' } }],
  ['quote_order', { fulfillment: [] }],
  ['quote_order', { fulfillment: { type: 'pickup', storeId: '1', secret: 1 }, pizzas: [] }],
  [
    'quote_order',
    {
      fulfillment: {
        type: 'delivery',
        address: { ID: 0, Name: 1, PostalCode: null, PostalCodeName: [], extra: 1 },
      },
    },
  ],
  [
    'quote_order',
    {
      fulfillment: { type: 'pickup', storeId: '1' },
      pizzas: 'bad',
      sides: [null],
      beverages: [{}],
      packages: [{}],
    },
  ],
  [
    'quote_order',
    {
      fulfillment: { type: 'pickup', storeId: '1' },
      pizzas: [{ quantity: 0, sizeId: '', crustId: 'bad/id', sections: [], extra: 1 }],
      extra: true,
    },
  ],
  [
    'quote_order',
    {
      fulfillment: { type: 'pickup', storeId: '1' },
      pizzas: [
        {
          sizeId: 'LG',
          crustId: 'HANDTOSS',
          sections: [
            {
              pizzaId: 'TEST',
              modifications: [
                { toppingId: 'BAD', quantity: 3 },
                { toppingId: 1, quantity: null, extra: 1 },
              ],
            },
          ],
        },
      ],
    },
  ],
  [
    'quote_order',
    {
      fulfillment: { type: 'pickup', storeId: '1' },
      packages: [
        { id: 'bundle', pizzas: [{}], sides: [{ id: 'side', extraId: null }], beverages: [{}] },
      ],
    },
  ],
  [
    'quote_order',
    {
      fulfillment: { type: 'pickup', storeId: '1', instructions: 'x'.repeat(301) },
      pizzas: [],
      coupon: '',
    },
  ],
  [
    'quote_order',
    {
      fulfillment: { type: 'pickup', storeId: '1' },
      sides: Array.from({ length: 21 }, () => ({ id: 'TEST' })),
    },
  ],
  ['create_checkout', {}],
  ['create_checkout', { quoteId: '../unsafe', expectedTotal: -1, extra: 1 }],
  ['create_checkout', { quoteId: null, expectedTotal: 1.5 }],
  [
    'create_checkout',
    { quoteId: '00000000-0000-0000-0000-000000000000', expectedTotal: 1_000_001 },
  ],
  ['get_checkout', { checkoutId: 42, extra: 1 }],
  ['get_checkout', { checkoutId: 'FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF' }],
  ['pay_saved_card', {}],
  ['pay_saved_card', { checkoutId: 'bad', cardId: 'card_x', expectedTotal: 1e30, confirm: false }],
  [
    'pay_saved_card',
    {
      checkoutId: '10000000-0000-4000-8000-000000000000',
      cardId: 1,
      expectedTotal: null,
      confirm: true,
      extra: 1,
    },
  ],
];
// ---- end of tests/ts/parity.ts ----

/** Values no output may hold: what the parity scripts asserted never escapes. */
const LEAKS = [
  'secret-',
  'private-',
  'synthetic-secret',
  'parity-access',
  'DO-NOT-RETURN',
  'rotated-access',
  'synthetic-access',
];

// ---- the fake: rust-dominos.ts `upstream`, recording each exchange ----
/** What the fake answers with now, and every exchange so far. */
class Fake {
  handler: Fetch = () => new Response(null, { status: 599 });
  exchanges: Exchange[] = [];
}

const fake = new Fake();

/** Bytes as UTF-8 text, a leading byte order mark kept; never lossy. */
function exact(bytes: Uint8Array): string {
  return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
}

async function bytesOf(request: IncomingMessage): Promise<Buffer> {
  const chunks: Buffer[] = [];

  for await (const chunk of request) if (Buffer.isBuffer(chunk)) chunks.push(chunk);

  return Buffer.concat(chunks);
}

async function serveFake(incoming: IncomingMessage, out: ServerResponse): Promise<void> {
  const path = incoming.url ?? '/';

  // Another process's loopback probe is never Domino’s.
  if (path === '/') {
    out.statusCode = 404;
    out.end();

    return;
  }

  const bytes = await bytesOf(incoming);
  const headers = new Headers();

  for (const [name, value] of Object.entries(incoming.headers))
    for (const item of [value ?? []].flat()) headers.append(name, item);

  const kept: Record<string, string> = {};

  for (const name of ['authorization', 'accept', 'content-type', 'content-length']) {
    const value = headers.get(name);

    if (value !== null) kept[name] = value;
  }

  const request: Seen = {
    method: incoming.method ?? 'GET',
    path,
    headers: kept,
    body: exact(bytes),
  };

  const url = path.startsWith('/website/')
    ? `https://www.dominos.is${path.slice(8)}`
    : path.startsWith('/adyen/')
      ? `https://checkoutshopper-live.adyen.com${path.slice(6)}`
      : `https://api.dominos.is${path}`;

  let response: Response;

  try {
    // rust-dominos.ts `body`: no body is `undefined`.
    const init: RequestInit = { method: request.method, headers, redirect: 'error' };

    if (bytes.length) init.body = bytes.toString('utf8');
    response = await fake.handler(url, init);
  } catch (error) {
    // A failed fetch: the connection closes without an answer. Shown, so an assertion of the
    // fixture provider is never mistaken for a mode's intended failure.
    process.stderr.write(`dropped ${request.method} ${path}: ${String(error)}\n`);
    fake.exchanges.push({ request, response: { status: 0, headers: {}, body: '' } });
    incoming.socket.destroy();

    return;
  }

  const body = new Uint8Array(await response.arrayBuffer());
  const answered: Record<string, string> = {};

  for (const [name, value] of response.headers) answered[name] = value;
  fake.exchanges.push({
    request,
    response: { status: response.status, headers: answered, body: exact(body) },
  });
  out.writeHead(response.status, answered);
  out.end(body);
}

const server = createServer((incoming, out) => void serveFake(incoming, out));

await new Promise<void>((done) =>
  server.listen({ host: '127.0.0.1', port: 0, exclusive: true }, done),
);

const address = server.address();

if (!(address instanceof Object)) throw new RangeError('No fake port.');

const origin = `http://127.0.0.1:${address.port}`;

// ---- steps ----
type Step =
  | { seed: { expired: boolean } }
  | { mode: string }
  | { file: string; text: string }
  | { dir: string; mode: number }
  | { cli: string[]; stdin?: string; env?: Record<string, string> }
  | { serve: [string, object][]; surface?: boolean };

/** One scratch home: its own environment (`<home>` in a value is the home) and steps. */
type Home = { env: Record<string, string>; steps: Step[] };

type Case = { name: string; homes: Home[] };

const SESSION = { DOMINOS_SESSION_FILE: '<home>/session.json' };

/** The handler for a recorded `mode` step. */
function handler(mode: string): Fetch {
  const [kind = '', name = ''] = mode.split(':');

  switch (kind) {
    case 'reads':
      return reads(new Provider());
    case 'failure':
      return failure(name);
    case 'login':
      return login(name);
    case 'money': {
      const scenario = scenarios.find((item) => item.name === name);

      if (!scenario) throw new RangeError(`No scenario ${name}.`);

      return money(scenario, new Provider());
    }

    case 'contract':
      return contractFetch(name);
    default:
      return () => {
        throw new Error('No request is expected.');
      };
  }
}

function slug(name: string): string {
  return name.toLowerCase().replaceAll(/[^a-z0-9]+/g, '-');
}

function moneyCase(scenario: Scenario): Case {
  const directory = 'session.json.checkouts';
  // money-parity.ts: a `create` scenario patches the quote, the others the checkout.
  const quoteRecord = { ...quote };
  const checkoutRecord = { ...checkout };
  Object.assign(scenario.tool === 'create' ? quoteRecord : checkoutRecord, scenario.patch);

  const steps: Step[] = [
    { seed: { expired: false } },
    { mode: `money:${scenario.name}` },
    {
      file: `${directory}/${Q}.json`,
      text: JSON.stringify(quoteRecord),
    },
    {
      file: `${directory}/${C}.json`,
      text: JSON.stringify(checkoutRecord),
    },
  ];

  if (scenario.mode === 'record-json')
    steps.push({ file: `${directory}/${C}.json`, text: 'secret-invalid-json' });

  if (scenario.mode === 'record-large')
    steps.push({ file: `${directory}/${C}.json`, text: 'x'.repeat(1048577) });
  steps.push({ serve: [moneyCall(scenario)] });

  return { name: slug(scenario.name), homes: [{ env: SESSION, steps }] };
}

const cases: Case[] = [
  {
    name: 'cli-argument-list',
    homes: [{ env: {}, steps: argumentLists.map((args) => ({ cli: args })) }],
  },
  {
    name: 'tool-surface-and-instructions',
    homes: [{ env: {}, steps: [{ serve: [], surface: true }] }],
  },
  { name: 'invalid-tool-input-list', homes: [{ env: {}, steps: [{ serve: invalid }] }] },
  {
    name: 'reads-parity',
    homes: [
      {
        env: SESSION,
        steps: [
          { seed: { expired: false } },
          { mode: 'reads' },
          ...operations.map((call): Step => ({ serve: [call] })),
          ...failures
            .filter((mode) => mode !== 'refresh-not-modified')
            .flatMap((mode): Step[] => [
              { mode: `failure:${mode}` },
              { serve: [mode.startsWith('menu-') ? ['search_menu', {}] : ['list_stores', {}]] },
            ]),
        ],
      },
      {
        env: SESSION,
        steps: [
          { seed: { expired: true } },
          { mode: 'failure:refresh-not-modified' },
          { serve: [['auth_status', {}]] },
        ],
      },
    ],
  },
  {
    name: 'auth-parity',
    homes: logins.map(([input, mode]) => ({
      env: {},
      steps: [{ mode: `login:${mode}` }, { cli: ['auth', 'login'], stdin: input }],
    })),
  },
  ...scenarios.map(moneyCase),
  {
    name: 'unsafe-config-folder-refusal',
    homes: [{ env: {}, steps: [{ dir: 'config/dominos-mcp', mode: 0o755 }, { cli: ['bad'] }] }],
  },
];

// ---- the contract cases the ledger names `dominos/contract/*` (41d6f98) ----
// Each drop-in case becomes the CLI runs and tool calls its binary-backed functions made; its
// scripted fetch is a fake mode of its own (`contractFetch`).

// tests/ts/catalog.test.ts:29: the menu with a second pizza and a two-pizza offer.
const offerMenu = parseMenu(html);

offerMenu.menuPizzas.push({ ...pizza, id: 'SECOND' });

offerMenu.packages.push({
  ...availability,
  id: 'BUNDLE',
  name: 'Two pizzas',
  description: '',
  price: 4000,
  availableForPickup: true,
  availableForDelivery: true,
  payOnlineOnly: true,
  items: [
    {
      id: 'pizza-slot',
      type: 0,
      quantity: 2,
      sizeId: 'LG',
      sizeList: ['LG'],
      pizzaType: 'HANDTOSS',
      isOptional: false,
      items: [
        { id: 'TEST', name: 'First' },
        { id: 'SECOND', name: 'Second' },
      ],
    },
  ],
});

/** catalog.test.ts's input, then each change it made before the next `orderItems`. */
function offerInputs(): object[] {
  const input = cartInput.parse({
    fulfillment: cart.fulfillment,
    packages: [
      {
        id: 'BUNDLE',
        pizzas: [
          {
            sizeId: 'LG',
            crustId: 'HANDTOSS',
            sections: [{ pizzaId: 'TEST' }],
            packageItemId: 'pizza-slot',
          },
          {
            sizeId: 'LG',
            crustId: 'HANDTOSS',
            sections: [{ pizzaId: 'SECOND' }],
            packageItemId: 'pizza-slot',
          },
        ],
      },
    ],
  });

  const inputs: object[] = [structuredClone(input)];
  const first = input.packages[0]?.pizzas[0];

  if (!first) throw new RangeError('No first pizza.');
  first.quantity = 2;
  inputs.push(structuredClone(input));
  first.quantity = 1;
  first.crustId = 'WRONG';
  inputs.push(structuredClone(input));
  first.crustId = 'HANDTOSS';
  first.sizeId = 'SM';
  inputs.push(structuredClone(input));
  first.sizeId = 'LG';
  first.sections = [{ pizzaId: 'UNKNOWN', modifications: [] }];
  inputs.push(structuredClone(input));

  return inputs;
}

function contractFetch(name: string): Fetch {
  const provider = new Provider();

  switch (name) {
    // reads.test.ts:10 and the drop-in's fixture client: the fixture provider.
    case 'provider':
      return provider.request;
    // reads.test.ts:10 `parseMenu('ReactDOM.hydrate({menu: malicious()})')` throws.
    case 'menu-call':
      return async (url, options) =>
        new URL(url).hostname === 'www.dominos.is'
          ? new Response('ReactDOM.hydrate({menu: malicious()})')
          : provider.request(url, options);
    // catalog.test.ts:29 `orderItems`.
    case 'offer':
      return async (url, options) => {
        if (new URL(url).hostname === 'www.dominos.is')
          return new Response(`ReactDOM.hydrate(${JSON.stringify({ menu: offerMenu })})`);

        if (new URL(url).pathname === '/api/orders')
          return Response.json({ Total: 4000, Success: true });

        return provider.request(url, options);
      };

    // auth.test.ts:70: the drop-in `login` answers its code request, the token request is 400.
    case 'login-rejected':
      return async (url) =>
        url.includes('/sendPin')
          ? new Response('')
          : new Response('secret-synthetic-response', { status: 400 });
    // auth.test.ts:70 `requestCode`: every request fails.
    case 'transport':
      return async () => {
        throw new Error('secret-synthetic-token');
      };

    // auth-integration.test.ts:181 `sessionStorage`: the drop-in answers the verification.
    case 'verify':
      return async () => Response.json({ id: 1, name: null, phoneNumber: '', email: null });
    default:
      throw new RangeError(`No contract fetch ${name}.`);
  }
}

const contracts: Case[] = [
  {
    // auth-integration.test.ts:181: `sessionStorage`, `migrate` and `logout` without a session.
    name: 'no-session-commands',
    homes: [
      {
        env: SESSION,
        steps: [
          { mode: 'contract:verify' },
          { cli: ['auth', 'status'] },
          { cli: ['auth', 'migrate'] },
          { cli: ['auth', 'logout'] },
        ],
      },
    ],
  },
  {
    // auth.test.ts:70: a rejected sign-in, then a code request that fails in transport.
    name: 'auth-errors-hide-credentials',
    homes: [
      {
        env: {},
        steps: [
          { mode: 'contract:login-rejected' },
          {
            cli: ['auth', 'login'],
            stdin: '5550123\n123456\n',
            env: { DOMINOS_SESSION_FILE: '<home>/session.json' },
          },
          { mode: 'contract:transport' },
          {
            cli: ['auth', 'login'],
            stdin: '5550123\n',
            env: { DOMINOS_SESSION_FILE: '<home>/config/dominos-code.json' },
          },
        ],
      },
    ],
  },
  {
    // catalog.test.ts:29: each `orderItems` in its own fixture home.
    name: 'offer-slots',
    homes: offerInputs().map((input) => ({
      env: SESSION,
      steps: [
        { seed: { expired: false } },
        { mode: 'contract:offer' },
        { serve: [['quote_order', input]] },
      ],
    })),
  },
  {
    // reads.test.ts:10: one connection lists the tools, reads the profile and the menu, and
    // refuses an unknown input; then the menu `parseMenu` refuses.
    name: 'public-menu-and-mcp-reads',
    homes: [
      {
        env: SESSION,
        steps: [
          { seed: { expired: false } },
          { mode: 'contract:provider' },
          {
            serve: [
              ['get_profile', {}],
              ['search_menu', { query: 'synthetic' }],
              ['quote_order', { ...cart, IsFinal: true }],
            ],
            surface: true,
          },
          { mode: 'contract:menu-call' },
          { serve: [['search_menu', {}]] },
        ],
      },
    ],
  },
];

// ---- recording ----
/** The environment of a run in `home`: scratch XDG folders, the seam, the fake. */
function environment(home: string, extra: Record<string, string>) {
  return {
    PATH: process.env.PATH ?? '/usr/bin:/bin',
    HOME: home,
    XDG_CONFIG_HOME: join(home, 'config'),
    XDG_DATA_HOME: join(home, 'data'),
    BUN_RUNTIME_TRANSPILER_CACHE_PATH: '0',
    FAMILY_MCP_STORE_TEST_SEAM: '1',
    DOMINOS_TEST_ORIGIN: origin,
    ...Object.fromEntries(
      Object.entries(extra).map(([key, value]) => [key, value.replaceAll('<home>', home)]),
    ),
  };
}

/** Run `work` with `home`'s environment in this process (the store paths follow it). */
async function inHome<T>(home: string, work: () => Promise<T>): Promise<T> {
  const before = { ...process.env };
  Object.assign(process.env, environment(home, SESSION));

  try {
    return await work();
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in before)) delete process.env[key];
    Object.assign(process.env, before);
  }
}

/** The session record's options in `home`, as the package's `sessionRecord` builds them. */
function record() {
  return {
    path: defaultSecretRecordPath('dominos-mcp'),
    server: 'dominos-mcp',
    profile: 'default',
    purpose: 'session',
    schema: 1,
    maxBytes: SESSION_MAX_BYTES,
    keys: defaultKeyProvider({ server: 'dominos-mcp', profile: 'default' }),
  };
}

/**
 * Per home, the random ids in the order they first appear, so the replay can name its own the
 * same way: `<uuid-1>`, `<uuid-2>`, ... The parity scripts' fixed ids keep their text.
 */
class Names {
  private readonly seen: string[] = [];

  constructor(private readonly home: string) {}

  get count(): number {
    return this.seen.length;
  }

  /**
   * `<home>` for the scratch home; `<uuid-N>` for a random v4 id; `<now±Nm>` for an epoch-ms
   * integer, and `<iso±Nm>` for an ISO time with milliseconds, within a day of now (N whole
   * minutes from now, rounded).
   */
  fresh(text: string): string {
    const now = Date.now();

    const minutes = (time: number) => {
      const offset = Math.round((time - now) / 60_000);

      return offset < 0 ? `${offset}m` : `+${offset}m`;
    };

    return text
      .replaceAll(realpathSync(this.home), '<home>')
      .replaceAll(this.home, '<home>')
      .replaceAll(/[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}/g, (id) => {
        if (id.startsWith('10000000-0000-4000-8000-')) return id;

        if (!this.seen.includes(id)) this.seen.push(id);

        return `<uuid-${this.seen.indexOf(id) + 1}>`;
      })
      .replaceAll(/(?<![\d.])1\d{12}(?![\d.])/g, (digits) => {
        const time = Number(digits);

        return Math.abs(time - now) < 86_400_000 ? `<now${minutes(time)}>` : digits;
      })
      .replaceAll(/\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z/g, (iso) => {
        const time = Date.parse(iso);

        return Math.abs(time - now) < 86_400_000 ? `<iso${minutes(time)}>` : iso;
      });
  }
}

function leaks(home: string, text: string): void {
  for (const leak of LEAKS)
    if (text.includes(leak)) throw new RangeError(`${home}: an output holds ${leak}.`);
}

/** No request names a random id, a time near now or the scratch home: requests are compared raw. */
function plain(home: string, exchanges: Exchange[]): Exchange[] {
  const text = JSON.stringify(exchanges.map((exchange) => exchange.request));

  if (new Names(home).fresh(text) !== text)
    throw new RangeError(`${home}: a request holds a random or time-dependent value.`);

  return exchanges;
}

async function runCli(
  home: string,
  names: Names,
  env: Record<string, string>,
  step: { cli: string[]; stdin?: string; env?: Record<string, string> },
) {
  const start = fake.exchanges.length;

  const child = Bun.spawn(
    [process.execPath, '--preload', rewrite, wrapper, JSON.stringify(step.cli)],
    {
      cwd: home,
      env: environment(home, { ...env, ...step.env }),
      stdin: step.stdin === undefined ? 'ignore' : new Blob([step.stdin]),
      stdout: 'pipe',
      stderr: 'pipe',
    },
  );

  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);

  leaks(home, stdout + stderr);

  return JSON.stringify({
    ...step,
    code,
    stdout: names.fresh(stdout),
    stderr: names.fresh(stderr),
    exchanges: plain(home, fake.exchanges.slice(start)),
  });
}

/** The protocol version and client the replay sends too. */
const INITIALIZE = {
  protocolVersion: '2025-11-25',
  capabilities: {},
  clientInfo: { name: 'dominos-parity', version: '1.0.0' },
};

type Params = typeof INITIALIZE | { name: string; arguments: unknown } | { cursor?: string };

type Outgoing = { jsonrpc: '2.0'; id?: number; method: string; params?: Params };

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
 * `dominos-mcp serve` over raw JSON-RPC: initialize, then each call in turn, then standard input
 * closes. Each answer is kept as the line the server wrote, with any notifications before it and
 * the exchanges the call caused.
 */
async function runServe(
  home: string,
  names: Names,
  env: Record<string, string>,
  step: { serve: [string, object][]; surface?: boolean },
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

  if (step.surface === true) {
    const list = await request('tools/list', {});
    parts.push(`"initialize":${initialize.answer}`, `"list":${list.answer}`);
  }

  const results: string[] = [];

  for (const [name, args] of step.serve) {
    const start = fake.exchanges.length;
    const { notifications, answer } = await request('tools/call', { name, arguments: args });
    leaks(home, answer + notifications.join(''));

    // parity.ts: every call of the invalid list must be an error.
    if (step.serve === invalid)
      assert.equal(
        z.object({ result: z.object({ isError: z.boolean() }) }).parse(JSON.parse(answer)).result
          .isError,
        true,
        `${name}: must be invalid`,
      );

    const before = notifications.length
      ? `"notifications":[${notifications.map((line) => names.fresh(line)).join(',')}],`
      : '';

    results.push(
      `{"call":${JSON.stringify([name, args])},${before}"answer":${names.fresh(answer)},"exchanges":${JSON.stringify(plain(home, fake.exchanges.slice(start)))}}`,
    );
  }

  await child.stdin.end();
  const [stderr, code] = await Promise.all([new Response(child.stderr).text(), child.exited]);
  leaks(home, stderr);
  parts.push(
    `"results":[\n${results.join(',\n')}\n]`,
    `"code":${code}`,
    `"stderr":${JSON.stringify(names.fresh(stderr))}`,
  );

  return `{"serve":true,${parts.join(',')}}`;
}

/** What the store holds: no store, the decrypted record text, or the error code. */
async function stored(home: string, names: Names) {
  return inHome(home, async () => {
    const options = record();

    if (!existsSync(options.path) && !existsSync(`${options.path}.marker`))
      return { store: 'none' };

    try {
      const text = await withSecretStore(options, (opened) => opened.read());

      return { record: text === null ? null : names.fresh(text) };
    } catch (error) {
      return { error: error instanceof SessionStoreError ? error.code : 'unknown' };
    }
  });
}

/**
 * Every path under `home` with its permission bits (a directory ends in `/`, a link names its
 * target), and the text of `session.json` and of each file under `session.json.checkouts`. Paths
 * are normalized like outputs and then sorted; a random id must have been named by an output first.
 */
function files(home: string, names: Names) {
  const before = names.count;

  const entries = readdirSync(home, { recursive: true, withFileTypes: true }).map((entry) => {
    const raw = relative(home, join(entry.parentPath, entry.name));
    const full = join(home, raw);
    const info = lstatSync(full);
    const mode = (info.mode & 0o777).toString(8);
    const path = names.fresh(raw);

    if (info.isSymbolicLink()) return { key: path, value: `${path} -> ${readlinkSync(full)}` };

    if (info.isDirectory()) return { key: path, value: `${path}/ ${mode}` };

    if (raw === 'session.json' || raw.startsWith('session.json.checkouts/'))
      return {
        key: path,
        value: { path: `${path} ${mode}`, text: names.fresh(readFileSync(full, 'utf8')) },
      };

    return { key: path, value: `${path} ${mode}` };
  });

  if (names.count !== before) throw new RangeError(`${home}: a file names an id no output named.`);

  return entries
    .toSorted((first, second) => (first.key < second.key ? -1 : first.key > second.key ? 1 : 0))
    .map((entry) => entry.value);
}

async function seed(home: string, expired: boolean): Promise<void> {
  await inHome(home, async () => {
    const path = join(home, 'session.json');
    await saveSession(path, saved(expired));
    assert.equal(await migrate(path), 'migrated');
  });
}

async function runHome(directory: string, spec: Home) {
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const home = realpathSync(directory);
  const names = new Names(home);
  const start = fake.exchanges.length;
  fake.handler = handler('none');
  const steps: string[] = [];

  for (const step of spec.steps) {
    if ('seed' in step) {
      await seed(home, step.seed.expired);
      steps.push(JSON.stringify(step));
    } else if ('cli' in step) steps.push(await runCli(home, names, spec.env, step));
    else if ('serve' in step) steps.push(await runServe(home, names, spec.env, step));
    else if ('dir' in step) {
      mkdirSync(join(home, step.dir), { recursive: true, mode: step.mode });
      steps.push(JSON.stringify(step));
    } else if ('file' in step) {
      // money-parity.ts plants records with `writePrivateFile`.
      await writePrivateFile(join(home, step.file), step.text);
      steps.push(JSON.stringify(step));
    } else {
      fake.handler = handler(step.mode);
      steps.push(JSON.stringify(step));
    }
  }

  const requests = fake.exchanges.length - start;
  const tail = { requests, store: await stored(home, names), files: files(home, names) };

  return `{"env":${JSON.stringify(spec.env)},"steps":[\n${steps.join(',\n')}\n],${JSON.stringify(tail).slice(1, -1)}}`;
}

/**
 * Long runs of one character (8 MiB bodies, 1 MiB records) inside JSON strings become
 * `\u0000<character>*<count>\u0000`; the replay expands them. No frozen value holds a NUL.
 */
function pack(text: string): string {
  if (text.includes('\\u0000')) throw new RangeError('A frozen value holds a NUL.');

  return text.replaceAll(
    /(?<!\\)([^\\"])\1{1023,}/g,
    (sequence: string, unit: string) => `\\u0000${unit}*${sequence.length}\\u0000`,
  );
}

// ---- store: what the TypeScript package left on disk for interop.test.ts ----
/** A fixed clock far ahead, so no frozen session, quote or checkout has expired when it is read. */
const FROZEN_NOW = Date.UTC(2100, 0, 1);

/** interop.test.ts `request`: its login's token exchange and account read. */
const interopRequest = async (url: string): Promise<Response> =>
  url.endsWith('/token')
    ? Response.json({
        access_token: 'interop-access',
        refresh_token: 'interop-refresh',
        username: '3545550123',
        token_type: 'bearer',
        expires_in: 3600,
      })
    : Response.json(profile);

/** fixtures.ts `setup()` on the TypeScript side: a session migrated into the store. */
async function migrated(path: string): Promise<void> {
  await saveSession(path, saved());
  assert.equal(await migrate(path), 'migrated');
}

/** The package's client in this process, against `provider`, for `work`. */
async function client<T>(
  path: string,
  provider: Provider,
  work: (reference: Reference) => Promise<T>,
): Promise<T> {
  const reference = new Reference(path, provider.request);

  try {
    return await work(reference);
  } finally {
    await reference.close();
  }
}

type StoreState = {
  name: string;
  case: string;
  run: (path: string, provider: Provider) => Promise<object>;
};

const payment = (created: { checkoutId: string; total: number }) => ({
  checkoutId: created.checkoutId,
  cardId: 'card_1',
  expectedTotal: created.total,
  confirm: true as const,
});

/** The TypeScript quote, then its checkout. */
async function quoteAndCheckout(reference: Reference) {
  const quoted = await reference.quoteOrder(cart);

  const created = await reference.createCheckout({
    quoteId: quoted.quoteId,
    expectedTotal: quoted.total,
  });

  return { quote: quoted, checkout: created };
}

const storeStates: StoreState[] = [
  {
    name: 'signed-in',
    case: 'interop.test.ts:23',
    run: async (path) => ({
      replaced: await referenceLogin('5550123', '123456', path, interopRequest),
    }),
  },
  {
    name: 'quoted',
    case: 'interop.test.ts:45',
    run: async (path, provider) => {
      await migrated(path);

      return client(path, provider, async (reference) => ({
        quote: await reference.quoteOrder(cart),
      }));
    },
  },
  {
    name: 'checkout-ready',
    case: 'interop.test.ts:45',
    run: async (path, provider) => {
      await migrated(path);

      return client(path, provider, quoteAndCheckout);
    },
  },
  {
    name: 'checkout-authorised',
    case: 'interop.test.ts:45',
    run: async (path, provider) => {
      await migrated(path);

      return client(path, provider, async (reference) => {
        const made = await quoteAndCheckout(reference);

        return { ...made, paid: await reference.paySavedCard(payment(made.checkout)) };
      });
    },
  },
];

/** Every file under `home`, sorted, with its permission bits and bytes. */
function storeFiles(home: string) {
  return readdirSync(home, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => {
      const path = join(entry.parentPath, entry.name);
      const bytes = readFileSync(path);
      const mode = (lstatSync(path).mode & 0o777).toString(8);
      const name = relative(home, path);

      return name.endsWith('.json')
        ? { path: name, mode, text: bytes.toString('utf8') }
        : { path: name, mode, hex: bytes.toString('hex') };
    })
    .toSorted((a, b) => (a.path < b.path ? -1 : 1));
}

async function freezeStores(directory: string): Promise<void> {
  const realNow = Date.now;
  Date.now = () => FROZEN_NOW;

  try {
    for (const state of storeStates) {
      const home = join(scratch, `store-${state.name}`);
      mkdirSync(home, { recursive: true, mode: 0o700 });
      const provider = new Provider();

      const result = await inHome(home, () => state.run(join(home, 'session.json'), provider));

      const frozen = {
        id: `dominos/store/${state.name}`,
        case: state.case,
        now: FROZEN_NOW,
        result,
        provider: { orders: provider.orders, payments: provider.payments },
        files: storeFiles(home),
      };

      const text = JSON.stringify(frozen, null, 1);

      if (text.includes(scratch)) throw new RangeError(`${state.name}: a value names the scratch.`);
      writeFileSync(join(directory, `${state.name}.json`), `${text}\n`);
      process.stdout.write(`${state.name}: ${frozen.files.length} files\n`);
    }
  } finally {
    Date.now = realNow;
  }
}

mkdirSync(output, { recursive: true });

try {
  for (const item of set === 'parity' ? cases : set === 'contract' ? contracts : []) {
    const homes: string[] = [];

    for (const [index, spec] of item.homes.entries())
      homes.push(await runHome(join(scratch, `${item.name}-${index}`), spec));
    const text = `{"id":${JSON.stringify(`dominos/${set}/${item.name}`)},"homes":[\n${homes.join(',\n')}\n]}`;

    if (text.includes(origin) || text.includes(scratch))
      throw new RangeError(`${item.name}: a recorded value names the fake or the scratch.`);
    JSON.parse(text);
    writeFileSync(join(output, `${item.name}.json`), `${pack(text)}\n`);
    process.stdout.write(`${item.name}: ${item.homes.length} homes\n`);
  }

  if (set === 'store') await freezeStores(output);
} finally {
  server.closeAllConnections();
  server.close();
  rmSync(scratch, { recursive: true, force: true });
}
