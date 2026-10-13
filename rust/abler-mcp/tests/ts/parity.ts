// TypeScript-versus-Rust parity for abler-mcp, driven by tests/parity.rs. Each scenario runs once
// against the TypeScript CLI (with rewrite.ts preloaded) and once against the Rust binary (built
// with `test-origin`), each in its own scratch home and against the same local fake upstream, and
// compares outputs, exit codes, upstream requests, the stored session and the files left behind.
// `interop` then shares one home between the two. Prints mismatches and exits 1 on any.
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

import {
  defaultKeyProvider,
  defaultSecretRecordPath,
  withSecretStore,
} from '../../../../packages/session-store/src/index.ts';

const [rust, only] = process.argv.slice(2);

if (!rust) throw new RangeError('Usage: parity.ts RUST_BINARY [SCENARIO]');

// Both sides keep their store in each scenario's scratch home through the store test seam.
if (process.env.FAMILY_MCP_STORE_TEST_SEAM !== '1')
  throw new RangeError('Tests must keep FAMILY_MCP_STORE_TEST_SEAM=1; the real store is never used.');
const repo = resolve(import.meta.dir, '../../../..');
const cli = join(repo, 'packages/abler-mcp/src/cli.ts');
const rewrite = join(import.meta.dir, 'rewrite.ts');
const scratch = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), 'abler-parity-'));

type Seen = { path: string; headers: Record<string, string>; body: string };

const fake = {
  mode: 'ok',
  accessAge: 3600,
  serial: 0,
  refresh: 'r0',
  access: '',
  seen: [] as Seen[],
  used: new Set<string>(),
};

function reset(): void {
  Object.assign(fake, { mode: 'ok', accessAge: 3600, serial: 0, refresh: 'r0', access: '' });
  fake.seen = [];
  fake.used = new Set();
}

function json(body: unknown, status = 200, setCookies: string[] = []): Response {
  const headers = new Headers({ 'Content-Type': 'application/json' });

  for (const cookie of setCookies) headers.append('Set-Cookie', cookie);

  return new Response(typeof body === 'string' ? body : JSON.stringify(body), { status, headers });
}

function rotate(): string[] {
  fake.serial += 1;
  fake.refresh = `r${fake.serial}`;
  fake.access = `a${fake.serial}`;

  return [
    `id_token=${fake.access}; Path=/; Max-Age=${fake.accessAge}; HttpOnly; Secure`,
    `refreshToken=${fake.refresh}; Path=/oauth; Max-Age=2592000; HttpOnly; Secure; SameSite=Lax`,
    '_ga=GA1.2; Path=/; Domain=abler.io',
  ];
}

const person = (id: string, displayName: string) => ({ id, displayName });

const event = (id: string, players: string[], ageGroup = 'g1') => ({
  eventId: id,
  name: `Practice ${id}`,
  type: 'TRAINING',
  description: id === 'e1' ? null : 'Bring water',
  from: '2026-10-10T17:00:00Z',
  to: id === 'e2' ? null : '2026-10-10T18:00:00Z',
  status: 'ACTIVE',
  arrivalTime: id === 'e3' ? '17:45' : 15,
  locationDetails: 'Hall',
  locationLink: null,
  ageGroup: { id: ageGroup, name: 'U10', extra: true },
  ...(id === 'e2' ? {} : { groups: [{ id: 's1', name: 'A' }] }),
  currentPlayerAttendance: players.map((player) => ({
    status: 'ATTENDING',
    coachStatus: player === '2' ? 'CONFIRMED' : null,
    player: person(player, `Player ${player}`),
  })),
  unknownField: 'dropped',
});

const message = {
  id: 'm1',
  messageBody: 'Hello <b>team</b>',
  createdAt: '2026-10-01T10:00:00Z',
  creator: person('u2', 'Coach'),
  attachments: [
    { id: 'f1', fileName: 'a.pdf', description: null, contentType: 'application/pdf', size: 3 },
  ],
  recipient: { isRead: false },
};

function data(operation: string, variables: Record<string, unknown>): unknown {
  const mode = fake.mode;

  switch (operation) {
    case 'SessionStatus':
      return { me: mode === 'badShape' ? { id: 'u1' } : { id: 'u1', displayName: 'Parent', email: 'x' } };
    case 'Profile': {
      const children = [person('c1', 'Ann'), { ...person('2', 'Bo'), age: 9 }, person('10', 'Cy')];

      if (mode === 'protoChild') children.push(person('constructor', 'K'), person('__proto__', 'P'));

      return { me: mode === 'meNull' ? null : { ...person('u1', 'Parent'), children } };
    }
    case 'Groups':
      return {
        me: {
          userAgeGroups: [
            {
              id: 'g1',
              name: 'U10',
              isActive: true,
              groups: [{ id: 's1', name: 'A', label: null }, { id: 's2', name: 'B' }],
              sport: { id: 'sp', name: 'Football', z: 1 },
            },
            { id: 'g2', name: 'U12', sport: null },
          ],
        },
      };
    case 'Schedule': {
      const cursor = variables.cursor as string | null;
      const filter = variables.filter as { participant?: string[] } | undefined;
      const participant = filter?.participant?.[0];
      const all = cursor === null ? [event('e1', ['c1', '2']), event('e2', ['c1'])] : [event('e3', ['2'])];
      const events = all.filter((e) => !participant || e.currentPlayerAttendance.some((a) => a.player.id === participant));
      const pageInfo =
        mode === 'stuckCursor'
          ? { hasNextPage: true, endCursor: cursor ?? 'n1' }
          : mode === 'incompleteCursor'
            ? { hasNextPage: true, endCursor: null }
            : cursor === null && events.length
              ? { hasNextPage: true, endCursor: 'n1' }
              : { hasNextPage: false, endCursor: null };

      return { schedule: { edges: events.map((node) => ({ node, cursor: 'x' })), pageInfo } };
    }
    case 'Event': {
      const edges =
        mode === 'eventMissing'
          ? []
          : [{ node: event(mode === 'eventMismatch' ? 'other' : String(variables.id), ['c1'], String(variables.ageGroupId)) }];

      return { event: { edges, pageInfo: { hasNextPage: false, endCursor: null } } };
    }
    case 'Conversations':
      return {
        getMessageUnreadCount: 3,
        message: {
          edges: [
            {
              node: {
                id: 'k1',
                name: 'Team',
                conversationType: 'GROUP',
                membersCount: 12,
                unreadCount: 2,
                messageGroup: { id: 'mg', name: 'U10' },
                user1: null,
                messages: { edges: [{ node: message }] },
              },
            },
            {
              node: {
                id: 'k2',
                name: null,
                conversationType: 'DIRECT',
                unreadCount: 1.5,
                user1: person('u1', 'Parent'),
                user2: person('u2', 'Coach'),
                messages: { edges: [] },
              },
            },
            { node: { id: 'k3', conversationType: 'DIRECT', unreadCount: 0, messages: null } },
          ],
          pageInfo: { hasNextPage: false, endCursor: 'k3' },
        },
      };
    case 'ConversationMessages':
      return {
        conversationMessages: {
          edges: [
            { node: message },
            {
              node: {
                id: 'm2',
                createdAt: '2026-10-02T10:00:00Z',
                messageBody: null,
                creator: null,
                attachments: null,
                recipient: { isRead: null },
              },
            },
            { node: { id: 'm3', createdAt: '2026-10-03T10:00:00Z', recipient: {} } },
          ],
          pageInfo: { hasNextPage: true, endCursor: 'm3' },
        },
      };
    default:
      return null;
  }
}

/** True the first time a once-only mode applies. */
function first(mode: string): boolean {
  if (fake.mode !== mode || fake.used.has(mode)) return false;
  fake.used.add(mode);

  return true;
}

async function handle(request: Request): Promise<Response> {
  const url = new URL(request.url);

  // Other processes probe loopback ports; only Abler's two endpoints are recorded.
  if (url.pathname !== '/oauth/token' && url.pathname !== '/graphql') return json({}, 404);
  const body = await request.text();
  const headers: Record<string, string> = {};

  for (const name of ['cookie', 'accept', 'content-type', 'content-length']) {
    const value = request.headers.get(name);

    if (value !== null) headers[name] = value;
  }
  fake.seen.push({ path: url.pathname, headers, body });
  const sent = Object.fromEntries(
    (request.headers.get('cookie') ?? '')
      .split('; ')
      .filter(Boolean)
      .map((pair) => [pair.slice(0, pair.indexOf('=')), pair.slice(pair.indexOf('=') + 1)]),
  );

  if (url.pathname === '/oauth/token') {
    switch (fake.mode) {
      case 'refresh401':
        return json({ error: 'invalid_grant' }, 401);
      case 'refresh403':
        return json({}, 403);
      case 'refresh500':
        return json({ error: 'secret detail' }, 500);
      case 'refreshNotJson':
        return json('<html>', 200);
      case 'refreshError':
        return json({ access_token: 't', error: 'x' }, 200, rotate());
      case 'refreshNoIdToken':
        return json({ access_token: 't' }, 200, rotate().slice(1));
      case 'refreshRedirect':
        return new Response(null, { status: 302, headers: { Location: '/elsewhere' } });
      case 'badCookieDomain':
        return json({ access_token: 't' }, 200, ['refreshToken=evil; Domain=evil.example; Path=/']);
    }

    if (sent.refreshToken !== fake.refresh) return json({ error: 'invalid_grant' }, 401);

    return json({ access_token: 't', token_type: 'Bearer' }, 200, rotate());
  }

  if (sent.id_token !== fake.access) return json({ errors: [{ message: 'unauthorized' }] }, 401);
  const { operationName, variables } = JSON.parse(body) as {
    operationName: string;
    variables: Record<string, unknown>;
  };

  if (first('graphql401Once')) return json({}, 401);

  if (first('unauthenticatedOnce'))
    return json({ errors: [{ message: 'x', extensions: { code: 'UNAUTHENTICATED' } }] });

  switch (fake.mode) {
    case 'graphqlErrors':
      return json({ data: data(operationName, variables), errors: [{ message: 'private detail' }] });
    case 'errorsCodeNumber':
      return json({ data: data(operationName, variables), errors: [{ extensions: { code: 5 } }] });
    case 'graphql500':
      fake.access = `e${++fake.serial}`;

      return json({ errors: [] }, 500, [`id_token=${fake.access}; Path=/; Max-Age=3600`]);
    case 'noData':
      return json({ data: null });
    case 'badJson':
      return json('{"data":');
    case 'bigBody':
      return json(`{"data":{"me":"${'a'.repeat(4 * 1024 * 1024)}"}}`);
  }

  return json({ data: data(operationName, variables) });
}

const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: handle });
const origin = `http://127.0.0.1:${server.port}`;

type Step =
  | { cli: string[]; stdin?: string }
  | { serve: [string, unknown][]; surface?: boolean }
  | { mode: string; accessAge?: number }
  | { file: string; text: string; permissions?: number };

type Scenario = { name: string; env?: Record<string, string>; steps: Step[] };

const exported = JSON.stringify([
  { name: 'refreshToken', value: 'r0', domain: '.abler.io', path: '/oauth', expirationDate: 4102444800, httpOnly: true },
  { name: 'id_token', value: 'stale', domain: 'www.abler.io', path: '/', expires: 1 },
  { name: '_ga', value: 'GA1', domain: '.abler.io' },
  { name: 'other', value: 'x', domain: 'example.com' },
]);

const legacy = JSON.stringify({
  version: 1,
  cookies: [
    { name: 'refreshToken', value: 'r0', domain: 'www.abler.io', path: '/oauth', expires: 4102444800, httpOnly: true, secure: true },
  ],
});

const importing: Step = { cli: ['auth', 'import', '-'], stdin: exported };

const sessionFile = '.config/abler-mcp/session.json';

const toolCalls: [string, unknown][] = [
  ['auth_status', {}],
  ['get_profile', {}],
  ['list_groups', {}],
  ['list_schedule', {}],
  ['list_schedule', { from: '2026-10-01', to: '2026-10-31', types: ['TRAINING'], groupIds: ['s1'], participantIds: ['c1'], first: 5, after: 'n1' }],
  ['list_child_schedules', {}],
  ['list_child_schedules', { childIds: ['2'], afterByChild: { '2': 'n1' }, types: [] }],
  ['list_child_schedules', { childIds: ['nope'] }],
  ['list_child_schedules', { childIds: ['c1'], afterByChild: { '2': 'n1' } }],
  ['get_event', { eventId: 'e7', ageGroupId: 'g9' }],
  ['list_conversations', {}],
  ['list_conversations', { first: 50, after: 'k0' }],
  ['list_messages', { conversationId: 'k1' }],
  ['list_messages', { conversationId: 'k1', first: 1, after: 'm0' }],
  ['nope', {}],
];

const invalidInputs: [string, unknown][] = [
  ['auth_status', { extra: 1 }],
  ['get_profile', { b: 1, a: 2, '1': 3 }],
  ['list_groups', { __proto__: 1 }],
  ['list_schedule', { first: 0 }],
  ['list_schedule', { first: 101, after: '' }],
  ['list_schedule', { first: 2.5 }],
  ['list_schedule', { first: 1e300 }],
  ['list_schedule', { first: '5' }],
  ['list_schedule', { from: '2026-02-30' }],
  ['list_schedule', { from: '2026-10-02', to: '2026-10-01' }],
  ['list_schedule', { from: '2026-10-02', to: '2026-10-01', x: 1 }],
  ['list_schedule', { from: '2026-10-02', to: '2026-10-01', first: 0 }],
  ['list_schedule', { from: 5, to: '2026-10-01' }],
  ['list_schedule', { types: 'TRAINING' }],
  ['list_schedule', { types: ['TRAINING', 'X'] }],
  ['list_schedule', { types: ['TRAINING', 'MATCH', 'GENERAL', 'CLASSES', 'MATCH'] }],
  ['list_schedule', { groupIds: [] }],
  ['list_schedule', { groupIds: [''] }],
  ['list_schedule', { participantIds: Array.from({ length: 21 }, (_, at) => `p${at}`) }],
  ['list_schedule', { after: 'x'.repeat(1025) }],
  ['list_schedule', { after: null }],
  ['list_child_schedules', { participantIds: ['c1'] }],
  ['list_child_schedules', { after: 'n1' }],
  ['list_child_schedules', { afterByChild: { c1: '' } }],
  ['list_child_schedules', { afterByChild: { '': 'n1' } }],
  ['list_child_schedules', { afterByChild: [] }],
  ['list_child_schedules', { childIds: [], first: 0, afterByChild: 1 }],
  ['get_event', {}],
  ['get_event', { eventId: 'e1' }],
  ['get_event', { eventId: '', ageGroupId: 'x'.repeat(257) }],
  ['list_conversations', { first: 51 }],
  ['list_messages', { first: 0 }],
  ['list_messages', { conversationId: 7, extra: true }],
  ['list_messages', { conversationId: '\u{1F600}'.repeat(129) }],
];

const refreshModes = ['refresh401', 'refresh403', 'refresh500', 'refreshNotJson', 'refreshError', 'refreshNoIdToken', 'refreshRedirect', 'badCookieDomain'];

const graphqlModes = ['graphql401Once', 'unauthenticatedOnce', 'graphqlErrors', 'errorsCodeNumber', 'graphql500', 'noData', 'badJson', 'bigBody', 'badShape', 'meNull', 'stuckCursor', 'incompleteCursor', 'eventMissing', 'eventMismatch', 'protoChild'];

const failingReads: [string, unknown][] = [
  ['auth_status', {}],
  ['get_profile', {}],
  ['list_schedule', { after: 'n1' }],
  ['get_event', { eventId: 'e1', ageGroupId: 'g1' }],
  ['list_child_schedules', {}],
  ['list_conversations', {}],
];

const scenarios: Scenario[] = [
  { name: 'surface', steps: [{ serve: [], surface: true }] },
  {
    name: 'no-session',
    steps: [
      { serve: [['auth_status', {}], ['get_profile', {}]] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'retry-candidate'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'logout'] },
      { cli: ['auth', 'status'] },
    ],
  },
  { name: 'tools', steps: [importing, { serve: toolCalls }, { cli: ['auth', 'status'] }] },
  { name: 'inputs', steps: [{ serve: invalidInputs }] },
  {
    name: 'cli',
    steps: [
      ['--help'], ['-h'], ['--version'], ['-v'], ['-hv'], ['--nope'], ['--help=1'], ['auth'],
      ['auth', 'nope'], ['auth', 'status', 'x'], ['auth', 'import'], ['auth', 'import', ''],
      ['--browser', 'x', 'auth', 'status'], ['auth', 'status', '--timeout', '5'], ['serve', 'x'],
      ['x'], ['auth', 'login', 'x'], ['auth', 'login', '--timeout', '0'], ['--timeout=1.5', 'auth', 'login'],
      ['auth', 'login', '--timeout', '-1'], ['auth', 'logout', 'x', 'y'], ['auth', 'retry-candidate', 'x'],
    ].map((args) => ({ cli: args })),
  },
  {
    name: 'import-errors',
    steps: [
      { cli: ['auth', 'import', '-'], stdin: '{"cookies":' },
      { cli: ['auth', 'import', '-'], stdin: JSON.stringify([{ name: 'id_token', value: 'a', domain: 'abler.io' }]) },
      { cli: ['auth', 'import', '-'], stdin: JSON.stringify([{ name: 'refreshToken', value: 'a b', domain: 'abler.io' }]) },
      { cli: ['auth', 'import', '-'], stdin: JSON.stringify({ cookies: [{ name: 'refreshToken', value: 'r0', domain: 'www.abler.io', path: 'x' }] }) },
      { cli: ['auth', 'import', '-'], stdin: ' '.repeat(4 * 1024 * 1024 + 1) },
      { cli: ['auth', 'import', 'missing.json'] },
      { file: 'loose.json', text: exported, permissions: 0o644 },
      { cli: ['auth', 'import', 'loose.json'] },
      { file: 'cookies.json', text: exported, permissions: 0o600 },
      { cli: ['auth', 'import', 'cookies.json'] },
      { cli: ['auth', 'status'] },
    ],
  },
  {
    name: 'verification',
    steps: [
      { mode: 'refresh500' },
      importing,
      { cli: ['auth', 'status'] },
      { mode: 'ok' },
      { cli: ['auth', 'retry-candidate'] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'retry-candidate'] },
    ],
  },
  { name: 'expired-import', steps: [{ mode: 'refresh401' }, importing, { cli: ['auth', 'retry-candidate'] }] },
  {
    name: 'migrate',
    steps: [
      { file: sessionFile, text: legacy },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { file: sessionFile, text: legacy },
      { file: `${sessionFile}.old.pending`, text: legacy },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'logout'] },
      { cli: ['auth', 'status'] },
    ],
  },
  {
    name: 'migrate-candidate',
    steps: [
      { file: `${sessionFile}.a.pending`, text: '{"broken":' },
      { file: `${sessionFile}.b.pending`, text: legacy },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'retry-candidate'] },
      { cli: ['auth', 'status'] },
    ],
  },
  {
    name: 'legacy-rotation',
    steps: [
      { mode: 'ok', accessAge: 30 },
      { file: sessionFile, text: legacy },
      { serve: [['get_profile', {}], ['auth_status', {}]] },
      { file: `${sessionFile}.bad`, text: '' },
      { file: sessionFile, text: '[]' },
      { serve: [['auth_status', {}]] },
    ],
  },
  {
    name: 'session-file-env',
    env: { ABLER_SESSION_FILE: 'custom/../custom/legacy.json' },
    steps: [{ file: 'custom/legacy.json', text: legacy }, { cli: ['auth', 'status'] }, { cli: ['auth', 'migrate'] }],
  },
  {
    name: 'collision',
    env: { ABLER_SESSION_FILE: '.config/abler-mcp/session.enc.json' },
    steps: [importing, { cli: ['auth', 'logout'] }],
  },
  ...refreshModes.map((mode) => ({
    name: `refresh-${mode}`,
    steps: [{ mode: 'ok', accessAge: 30 }, importing, { mode }, { serve: [['auth_status', {}], ['auth_status', {}]] }] as Step[],
  })),
  ...graphqlModes.map((mode) => ({
    name: `graphql-${mode}`,
    steps: [importing, { mode }, { serve: failingReads }] as Step[],
  })),
];

function environment(home: string, extra: Record<string, string> = {}): Record<string, string> {
  return {
    PATH: process.env.PATH ?? '/usr/bin:/bin',
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_DATA_HOME: join(home, '.local/share'),
    XDG_STATE_HOME: join(home, '.local/state'),
    XDG_CACHE_HOME: join(home, '.cache'),
    BUN_RUNTIME_TRANSPILER_CACHE_PATH: '0',
    FAMILY_MCP_STORE_TEST_SEAM: '1',
    ABLER_TEST_ORIGIN: origin,
    ...extra,
  };
}

function command(side: 'ts' | 'rust', args: string[]): string[] {
  return side === 'ts' ? [process.execPath, '--preload', rewrite, cli, ...args] : [rust!, ...args];
}

async function runCli(side: 'ts' | 'rust', home: string, env: Record<string, string>, args: string[], stdin?: string) {
  const child = Bun.spawn(command(side, args), {
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

  // Rust's command help now describes its additional tools. Keep comparing CLI behavior
  // and errors while dedicated tests cover the help text.
  const helpOnly = args.length === 1 && ['--help', '-h', '-hv', 'auth'].includes(args[0]!);
  return { args, code, stdout: helpOnly ? '<help output>' : stdout, stderr };
}

async function runServe(side: 'ts' | 'rust', home: string, env: Record<string, string>, calls: [string, unknown][], surface = false) {
  const [executable, ...args] = command(side, ['serve']);
  const transport = new StdioClientTransport({ command: executable!, args, cwd: home, env: environment(home, env), stderr: 'pipe' });
  const client = new Client({ name: 'abler-parity', version: '1.0.0' });
  const results: unknown[] = [];
  await client.connect(transport);

  if (surface) {
    results.push({
      server: client.getServerVersion(),
      capabilities: client.getServerCapabilities(),
      // Rust adds guarded discovery and fixed confirmed actions absent from the TypeScript
      // reference package. The new guidance is covered by Rust surface tests.
      // Rust-only discovery and fixed write tools have their own tests. Compare the
      // pre-existing TypeScript tools' descriptions, schemas and annotations unchanged.
      tools: (await client.listTools()).tools.filter(tool => !['describe_api','mark_conversation_read','mark_all_read','reply_to_conversation','set_child_attendance','join_event','leave_event'].includes(tool.name)),
    });
  }

  for (const [name, args] of calls) {
    try {
      results.push(await client.callTool({ name, arguments: args as Record<string, unknown> }));
    } catch (error) {
      results.push({ protocolError: error instanceof Error ? error.message : String(error) });
    }
  }
  await client.close();

  return results;
}

/** Relative expiry in 100 s steps, so both sides compare although they ran moments apart. */
function cookies(list: { expires?: number }[]): unknown[] {
  const now = Date.now() / 1000;

  return list.map((cookie) => ({
    ...cookie,
    expires: cookie.expires && cookie.expires > 0 ? Math.round((cookie.expires - now) / 100) : cookie.expires,
  }));
}

async function stored(home: string): Promise<unknown> {
  const saved = { ...process.env };
  Object.assign(process.env, environment(home));

  try {
    const options = {
      path: defaultSecretRecordPath('abler-mcp'),
      server: 'abler-mcp',
      profile: 'default',
      purpose: 'session',
      schema: 1,
      maxBytes: 262_144 * 2 + 4096,
      keys: defaultKeyProvider({ server: 'abler-mcp', profile: 'default' }),
    };

    if (!existsSync(options.path) && !existsSync(`${options.path}.marker`)) return 'no store';
    const text = await withSecretStore(options, (store) => store.read());

    if (text === null) return null;
    const record = JSON.parse(text) as {
      current: { cookies: { expires?: number }[] } | null;
      candidate: { id: string; jar: { cookies: { expires?: number }[] } } | null;
    };

    return {
      current: record.current && cookies(record.current.cookies),
      candidate: record.candidate && { id: typeof record.candidate.id, cookies: cookies(record.candidate.jar.cookies) },
    };
  } catch (error) {
    return `error: ${error instanceof Error && 'code' in error ? String(error.code) : 'unknown'}`;
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved);
  }
}

function files(home: string): unknown {
  const directory = join(home, '.config/abler-mcp');

  if (!existsSync(directory)) return [];

  return readdirSync(directory)
    .sort()
    .map((name) => {
      if (!name.endsWith('.json')) return name;
      const text = readFileSync(join(directory, name), 'utf8');

      try {
        const parsed = JSON.parse(text) as { cookies?: { expires?: number }[] };

        return { name, cookies: parsed.cookies && cookies(parsed.cookies) };
      } catch {
        return { name, text };
      }
    });
}

function write(home: string, step: { file: string; text: string; permissions?: number }): void {
  const path = join(home, step.file);
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  writeFileSync(path, step.text, { mode: step.permissions ?? 0o600 });
  chmodSync(path, step.permissions ?? 0o600);
}

async function run(side: 'ts' | 'rust', scenario: Scenario) {
  const home = join(scratch, scenario.name, side);
  mkdirSync(home, { recursive: true, mode: 0o700 });
  reset();
  const steps: unknown[] = [];

  for (const step of scenario.steps) {
    if ('cli' in step) steps.push(await runCli(side, home, scenario.env ?? {}, step.cli, step.stdin));
    else if ('serve' in step) steps.push(await runServe(side, home, scenario.env ?? {}, step.serve, step.surface));
    else if ('mode' in step) Object.assign(fake, { mode: step.mode, accessAge: step.accessAge ?? fake.accessAge });
    else write(home, step);
  }

  return { steps, requests: fake.seen, store: await stored(home), files: files(home) };
}

const failures: string[] = [];

/** Successful tool results and upstream requests on the TypeScript side, so parity is not vacuous. */
const coverage = { results: 0, requests: 0 };

function compare(name: string, ts: unknown, rs: unknown, path = ''): void {
  if (JSON.stringify(ts) === JSON.stringify(rs)) return;

  if (ts && rs && typeof ts === 'object' && typeof rs === 'object' && Array.isArray(ts) === Array.isArray(rs)) {
    const keys = [...new Set([...Object.keys(ts), ...Object.keys(rs)])];

    if (JSON.stringify(Object.keys(ts)) !== JSON.stringify(Object.keys(rs)))
      failures.push(`${name}${path}: keys ${JSON.stringify(Object.keys(ts))} != ${JSON.stringify(Object.keys(rs))}`);

    for (const key of keys)
      compare(name, (ts as Record<string, unknown>)[key], (rs as Record<string, unknown>)[key], `${path}.${key}`);

    return;
  }
  const clip = (value: unknown) => String(JSON.stringify(value)).slice(0, 600);
  failures.push(`${name}${path}:\n  ts:   ${clip(ts)}\n  rust: ${clip(rs)}`);
}

try {
  for (const scenario of scenarios) {
    if (only && scenario.name !== only) continue;
    const ts = await run('ts', scenario);
    coverage.requests += ts.requests.length;
    coverage.results += JSON.stringify(ts.steps).split('"structuredContent"').length - 1;
    compare(scenario.name, ts, await run('rust', scenario));
  }

  if (!only || only === 'interop') {
    // One home: each language imports, the other's server rotates on every read, and the first
    // reads the rotation back. The fake only accepts the newest refresh token.
    for (const [first, second] of [['ts', 'rust'], ['rust', 'ts']] as const) {
      const home = join(scratch, `interop-${first}`);
      mkdirSync(home, { recursive: true, mode: 0o700 });
      reset();
      fake.accessAge = 30;
      const imported = await runCli(first, home, {}, ['auth', 'import', '-'], exported);
      const served = await runServe(second, home, {}, [['get_profile', {}], ['list_groups', {}]]);
      const status = await runCli(first, home, {}, ['auth', 'status']);
      const rotations = fake.serial;
      const ok =
        imported.code === 0 &&
        served.every((result) => !(result as { isError?: boolean }).isError) &&
        status.code === 0 &&
        JSON.parse(status.stdout).authenticated === true &&
        rotations === 4;

      if (!ok) failures.push(`interop ${first} -> ${second}: ${JSON.stringify({ imported, served, status, rotations })}`);
    }
  }
} finally {
  await server.stop(true);
  rmSync(scratch, { recursive: true, force: true });
}

if (!only && (coverage.results < 40 || coverage.requests < 150))
  failures.push(`coverage too low: ${JSON.stringify(coverage)}`);

if (failures.length) {
  console.log(failures.join('\n'));
  process.exit(1);
}
console.log(`parity ok: ${JSON.stringify(coverage)}`);
