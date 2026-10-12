// Freezes what the TypeScript Inna CLI and server do in every case of
// rust/inna-mcp/tests/ts/parity.ts, and in the drop-in cases the ledger names `inna/contract/*`,
// for rust/inna-mcp/tests/replay.rs. Run once per set from the repository root with Bun 1.4.2 at
// the commit named in the provenance:
//
//   BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 \
//     bun tooling/ts-freeze/inna-parity.ts rust/inna-mcp/tests/fixtures/parity
//   BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 \
//     bun tooling/ts-freeze/inna-parity.ts rust/inna-mcp/tests/fixtures/contract contract
//
// parity.ts ran each scenario once against the TypeScript CLI (rewrite.ts preloaded) and once
// against the binary, each in its own scratch home, against one fake Inna (fake-inna.ts) and one
// clock file. This runs the TypeScript side alone, every step of it: a `swap` step, which ran the
// binary in the TypeScript home, runs the TypeScript server here, and the home as it stood before
// that step is recorded byte for byte, so that the replay plants what the TypeScript side wrote and
// the binary continues from it. Between the fake and the CLI sits a recording proxy, which keeps
// each upstream exchange in order: the request (method, URL, the headers fake-inna.ts compares,
// body) and the response the fake served (status, headers, body). For a GET answered 401 it also
// asks a copy of the fake's state what the renewal and the one retry parity.ts let the binary send
// would be answered, without changing the fake. MCP traffic is raw newline-delimited JSON-RPC, and
// each answer is kept as the line the server wrote. The scenario tables are parity.ts's, copied
// between the markers below; the copied code changed only where this workspace's lint requires
// (marked `lint:`) and in layout (oxfmt), not in behaviour.
//
// A contract case is its test's steps as the drop-in client made them (a tool call on one long
// `serve`, an `auth` command, a sign-in), run against the TypeScript side with the test's own
// scripted Inna (integration.test.ts's `Provider`, login.test.ts's `provider()` and rust-login.ts's
// `user()`, copied unchanged between markers) behind the same proxy, and asserting what the test
// asserted. Every value is synthetic. PR 3 deletes this script with the TypeScript sources.
import { strict as assert } from 'node:assert';
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import * as z from 'zod/v4';

import { expect, setSystemTime } from 'bun:test';

import {
  SessionStoreError,
  defaultKeyProvider,
  defaultSecretRecordPath,
  withSecretRecord,
  withSecretStore,
} from '../../packages/session-store/src/index.ts';
import {
  RECORD_MAX_BYTES,
  cookieExportSchema,
  sessionJar,
  type InnaClient,
} from '../../packages/inna-mcp/src/client.ts';
import type { User } from '../../packages/inna-mcp/src/schemas.ts';
import {
  COOKIES,
  fresh,
  startFake,
  type Planted,
  type Seen,
  type State,
} from '../../rust/inna-mcp/tests/ts/fake-inna.ts';

const [output, set = 'parity', only] = process.argv.slice(2);

if (output === undefined || !['parity', 'contract'].includes(set))
  throw new RangeError('Usage: inna-parity.ts OUTPUT_DIRECTORY [parity|contract] [CASE]');

if (existsSync(output) && readdirSync(output).length)
  throw new RangeError('The output directory must be empty or absent.');

if (process.env.FAMILY_MCP_STORE_TEST_SEAM !== '1')
  throw new RangeError('Keep FAMILY_MCP_STORE_TEST_SEAM=1; the real store is never used.');

const repo = resolve(import.meta.dir, '../..');

const cli = join(repo, 'packages/inna-mcp/src/cli.ts');

// Routes the package's fetch to the proxy and its `Date.now` to the clock file; the contract set's
// copy routes the electronic-ID host too.
const rewrite =
  set === 'contract'
    ? join(repo, 'tooling/ts-freeze/inna-rewrite.ts')
    : join(repo, 'rust/inna-mcp/tests/ts/rewrite.ts');

const scratch = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), 'ts-freeze-inna-'));

const clock = join(scratch, 'now');

const NOW = Date.parse('2040-01-02T12:00:00Z');

type Current = { state: State; seen: Seen[] };

let current: Current = { state: fresh(), seen: [] };

const fake = await startFake(() => current);

type Answer = { status: number; headers: Record<string, string | string[]>; body: string };

type Exchange = {
  request: { method: string; url: string; headers: Record<string, string | null>; body: string };
  response?: Answer;
  /** What the renewal parity.ts let the binary send after this 401, and its one retry, got. */
  recovery?: { renewal: Answer; retry?: Answer };
};

/** fake-inna.ts COMPARED_HEADERS: what each request carried that both sides must agree on. */
const COMPARED = [
  'accept',
  'authorization',
  'content-type',
  'cookie',
  'origin',
  'referer',
  'x-requested-by',
  'x-xsrf-token',
];

/** Every exchange the proxy has seen, in the order the requests arrived. */
const exchanges: Exchange[] = [];

/** What went wrong out of a case's sight (a provider's failed expectation); the freeze fails. */
const problems: string[] = [];

/** Where the proxy forwards: the parity fake, or the contract set's case upstream. */
let target = fake.origin;

/** A request's body as UTF-8 text, a leading byte order mark kept; never lossy. */
async function bytesOf(request: IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];

  for await (const chunk of request) if (Buffer.isBuffer(chunk)) chunks.push(chunk);

  return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(Buffer.concat(chunks));
}

/** The fake's answer to `method path`, without the transport's own headers. */
async function forward(
  method: string,
  path: string,
  headers: Headers,
  body: string,
): Promise<Answer> {
  const init: RequestInit = { method, headers, redirect: 'manual' };

  if (method !== 'GET' && method !== 'HEAD') init.body = body;

  const response = await fetch(`${target}${path}`, init);

  const kept: Record<string, string | string[]> = {};

  for (const [name, value] of response.headers)
    if (
      ![
        'set-cookie',
        'date',
        'connection',
        'keep-alive',
        'content-length',
        'transfer-encoding',
      ].includes(name)
    )
      kept[name] = value;

  const cookies = response.headers.getSetCookie();

  if (cookies.length) kept['set-cookie'] = cookies;

  return { status: response.status, headers: kept, body: await response.text() };
}

async function proxy(incoming: IncomingMessage, out: ServerResponse): Promise<void> {
  const path = incoming.url ?? '/';
  const [, host = '', ...rest] = path.split('/');
  const url = new URL(`https://${host}/${rest.join('/')}`).href;
  const method = incoming.method ?? '';
  const recorded: Record<string, string | null> = {};

  for (const name of COMPARED) recorded[name] = incoming.headers[name]?.toString() ?? null;

  const headers = new Headers();

  for (const [name, value] of Object.entries(incoming.headers))
    if (!['host', 'connection', 'content-length'].includes(name))
      for (const item of [value ?? []].flat()) headers.append(name, item);

  const body = await bytesOf(incoming);
  const exchange: Exchange = { request: { method, url, headers: recorded, body } };
  exchanges.push(exchange);
  let answer: Answer;

  try {
    answer = await forward(method, path, headers, body);
  } catch {
    // The upstream dropped the connection, as a failed fetch: status 0, and so does the proxy.
    exchange.response = { status: 0, headers: {}, body: '' };
    incoming.socket.destroy();

    return;
  }

  exchange.response = answer;

  if (method === 'GET' && answer.status === 401 && set === 'contract')
    problems.push(`${url}: a GET answered 401 in a contract case, which records no recovery`);
  else if (method === 'GET' && answer.status === 401) {
    // A copy of the fake's state answers the renewal and the retry, so the TypeScript run sees no
    // change.
    const kept = current;
    current = { state: structuredClone(kept.state), seen: [] };

    try {
      const renewal = await forward(
        'GET',
        '/nam.inna.is/api/UserData/GetLoggedInUser',
        headers,
        '',
      );

      exchange.recovery =
        renewal.status === 200
          ? { renewal, retry: await forward(method, path, headers, body) }
          : { renewal };
    } finally {
      current = kept;
    }
  }

  try {
    out.writeHead(answer.status, answer.headers);
    out.end(answer.body);
  } catch {
    // The client has gone (a cancelled call); the exchange is recorded all the same.
  }
}

const proxyServer = createServer((incoming, out) => void proxy(incoming, out));

await new Promise<void>((done) => proxyServer.listen({ host: '127.0.0.1', port: 0 }, done));

const address = proxyServer.address();

if (!(address instanceof Object)) throw new RangeError('No proxy port.');

const origin = `http://127.0.0.1:${address.port}`;

// parity.ts's LEGACY session holds a cookie jar whose creation times are the wall clock's; at the
// scenario clock instead, the frozen legacy file is the same on every run.
setSystemTime(new Date(NOW));

// ---- parity.ts (41d6f98): steps, inputs and scenarios ----
type Step =
  // `absent`: text neither side may print in this step.
  | { cli: string[]; stdin?: string; absent?: string }
  // Each side's `auth import` of the synthetic cookie export.
  | { seed: true; args?: string[] }
  // An older version's plaintext session file at this path under the home.
  | { legacy: string }
  | { upstream: Partial<State> }
  | { clock: number }
  | { serve: [string, unknown][]; args?: string[]; surface?: boolean; swap?: boolean }
  // One call, cancelled once Inna has seen a request to `when`: by the host
  // (`notifications/cancelled`), by the end of the server's stdin, or by a signal to it. `writes`:
  // the absence writes Inna must have received by the end of the step.
  | {
      cancel: [string, unknown];
      by: 'host' | 'stdin' | 'SIGINT' | 'SIGTERM';
      when: string;
      writes: number;
      args?: string[];
    }
  | { file: string; text: string; permissions?: number }
  | { directory: string; permissions: number };

type Scenario = { name: string; env?: Record<string, string>; steps: Step[] };

const storeFile = '.config/inna-mcp/session.enc';

/** A saved session as an older version left it in plaintext, with the synthetic cookies. */
const LEGACY = JSON.stringify({
  version: 2,
  jar: JSON.stringify(
    await (await sessionJar(cookieExportSchema.parse(JSON.parse(COOKIES)))).serialize(),
  ),
  account: { userId: 1, studentId: '2', schoolId: '3' },
  students: {},
  pauseUntil: 0,
});

const cookieFile = (cookies: object[]) => JSON.stringify(cookies);

const range = { dateFrom: '2040-01-02', dateTo: '2040-01-09' };

/** Every read tool, with each argument that changes what it asks Inna. */
const READS: [string, unknown][] = [
  ['inna_session_status', {}],
  ['inna_list_students', {}],
  ['inna_get_overview', {}],
  ['inna_get_timetable', range],
  ['inna_get_assignments', {}],
  ['inna_get_assignments', { type: 'exams' }],
  ['inna_get_assignments', { type: 'assignments' }],
  ['inna_get_assignment', { assignmentId: '5' }],
  ['inna_get_grades', {}],
  ['inna_get_grades', { termId: '4' }],
  ['inna_get_course_grades', { groupId: '7' }],
  ['inna_get_attendance', {}],
  ['inna_get_attendance', { termId: '4' }],
  ['inna_get_materials', { groupId: '7' }],
  ['inna_get_messages', {}],
  ['inna_get_messages', { rowFrom: 2, rowTo: 40 }],
  ['inna_get_message', { messageId: '11', type: 'A' }],
  ['inna_get_absences', range],
  ['inna_absence_status', {}],
];

const SIBLING_READS: [string, unknown][] = [
  ['inna_get_overview', { studentKey: '5' }],
  ['inna_session_status', { studentKey: '5' }],
  ['inna_list_students', {}],
  ['inna_get_timetable', { ...range, studentKey: '5' }],
  ['inna_get_absences', { ...range, studentKey: '5' }],
  ['inna_session_status', {}],
  ['inna_get_overview', { studentKey: '77' }],
  ['inna_get_overview', { studentKey: '9' }],
  ['inna_get_overview', {}],
];

const INVALID: [string, unknown][] = [
  ['inna_get_timetable', {}],
  ['inna_get_timetable', { dateFrom: '2040-01-03', dateTo: '2040-01-02' }],
  ['inna_get_timetable', { dateFrom: 'x', dateTo: '2040-01-02', q: 1 }],
  ['inna_get_absences', { dateFrom: '2040-02-30', dateTo: '2040-03-01' }],
  ['inna_get_assignments', { type: 'x' }],
  ['inna_get_assignment', { assignmentId: 'x' }],
  ['inna_get_assignment', { assignmentId: '1'.repeat(33) }],
  ['inna_get_grades', { termId: 5 }],
  ['inna_get_course_grades', {}],
  ['inna_get_messages', { rowFrom: 0 }],
  ['inna_get_messages', { rowFrom: 5, rowTo: 2 }],
  ['inna_get_messages', { rowFrom: 1, rowTo: 500 }],
  ['inna_get_messages', { rowFrom: 1.5 }],
  ['inna_get_message', { messageId: '11', type: 'a' }],
  ['inna_get_overview', { studentKey: 'x' }],
  ['inna_list_students', { x: 1 }],
  ['inna_session_status', { studentKey: 5 }],
  ['inna_absence_status', { x: 1 }],
];

/** Each failure Inna can answer one read with, then the same read once it answers again. */
const FAILURES: Step[] = [
  ...[
    { status: 401 },
    { status: 403 },
    { status: 302, headers: { location: 'https://example.invalid/credential-trap' } },
    { status: 500, body: 'Synthetic failure' },
    { status: 200, body: '<html>Synthetic</html>', headers: { 'content-type': 'text/html' } },
    { status: 200, body: '{}' },
    { status: 200, body: '{"assignmentId":' },
    { status: 404 },
  ].flatMap((planted) => [
    { upstream: { planted: { '/api/GetAssignments/GetAssignmentInfo': planted } } },
    // lint: `satisfies`, not an assertion.
    { serve: [['inna_get_assignment', { assignmentId: '5' }]] satisfies [string, unknown][] },
  ]),
  { upstream: { planted: { '/api/UserData/GetLoggedInUser': { status: 401 } } } },
  {
    serve: [
      ['inna_session_status', {}],
      ['inna_get_overview', {}],
      ['inna_list_students', {}],
    ],
  },
  // Rate limits last: each pause is saved, and the clock moves past it.
  ...['60', 'Fri, 03 Jan 2040 12:10:00 GMT', '', 'soon', '99999999'].flatMap((after, index) => [
    { clock: NOW + index * 86_400_000 },
    {
      upstream: {
        planted: {
          '/api/GetAssignments/GetAssignmentInfo': {
            status: 429,
            headers: after ? { 'retry-after': after } : {},
          },
        },
      },
    },
    // lint: `satisfies`, not an assertion.
    {
      serve: [
        ['inna_get_assignment', { assignmentId: '5' }],
        ['inna_get_overview', {}],
      ] satisfies [string, unknown][],
    },
  ]),
  { upstream: { planted: {} } },
  { serve: [['inna_get_overview', {}]] },
  // Past the longest pause, about three years.
  { clock: NOW + 4 * 365 * 86_400_000 },
  { serve: [['inna_get_overview', {}]] },
];

const WRITES = ['--allow-absence-writes'];

/** Fields a request adds or overrides; `q` is one the tools must refuse. */
type Extra = { kind?: string; dateTo?: string; reason?: string; studentKey?: string; q?: number };

// lint: the extra fields as a named type, not `object`.
const sick = (day: string, extra: Extra = {}) => ({
  kind: 'sick',
  dateFrom: day,
  dateTo: day,
  reason: ' Synthetic reason ',
  ...extra,
});

// lint: the extra fields as a named type, not `object`.
const leave = (dateFrom: string, dateTo: string, extra: Extra = {}) => ({
  kind: 'leave',
  dateFrom,
  dateTo,
  reason: 'Synthetic leave',
  ...extra,
});

const submit = (operationId = '<operation>'): [string, unknown] => [
  'inna_submit_absence',
  { operationId, confirm: true },
];

/** Inna's absence history, empty, and its answer to a write. */
const clear = (
  write: Planted = { status: 200, body: '{"id":123,"extra":"x"}' },
): Partial<State> => ({
  planted: {
    '/api/RegisterAbsence/GetStudentRegisteredAbsences': { status: 200, body: '[]' },
    '/api/RegisterAbsence/GetLeaves': { status: 200, body: '[]' },
    '/api/RegisterAbsence/AddNewLeave': write,
  },
});

const ABSENCE_INVALID: [string, unknown][] = [
  ['inna_prepare_absence', {}],
  ['inna_prepare_absence', sick('2040-01-02', { dateTo: '2040-01-03' })],
  ['inna_prepare_absence', leave('2040-01-03', '2040-01-02', { reason: '  ', q: 1 })],
  ['inna_prepare_absence', sick('2040-02-30', { kind: 'x', studentKey: 'x' })],
  ['inna_prepare_absence', sick('2040-01-02', { reason: 'x'.repeat(2001) })],
  ['inna_submit_absence', {}],
  ['inna_submit_absence', { operationId: 'x', confirm: false }],
  [
    'inna_submit_absence',
    { operationId: '123e4567-e89b-12d3-a456-426614174000', confirm: true, x: 1 },
  ],
];

const scenarios: Scenario[] = [
  { name: 'surface', steps: [{ serve: [], surface: true }] },
  {
    name: 'surface-writes',
    steps: [{ serve: [], args: ['--allow-absence-writes'], surface: true }],
  },
  {
    name: 'surface-no-keep-alive',
    steps: [{ serve: [], args: ['--no-keep-alive'], surface: true }],
  },
  {
    name: 'write-tools-hidden',
    steps: [
      {
        serve: [
          ['inna_prepare_absence', {}],
          ['inna_submit_absence', {}],
          ['nope', {}],
        ],
      },
    ],
  },
  {
    name: 'cli',
    steps: [
      ['--help'],
      ['-h'],
      ['--version'],
      ['-v'],
      ['-hv'],
      ['--help', 'x', '--google'],
      ['--nope'],
      ['-x'],
      ['--help=1'],
      ['--google=1'],
      ['--timeout'],
      ['--timeout', '-5'],
      ['--keep-alive'],
      ['auth'],
      ['x'],
      [''],
      ['serve', 'x'],
      ['serve', '--allow-account-change'],
      ['serve', '--google'],
      ['--timeout', '5'],
      ['auth', 'login', '--timeout', '5'],
      ['auth', 'login', '--browser', 'b'],
      ['auth', 'login', 'x', '--google'],
      ['auth', 'status', '--google'],
      ['auth', 'login', '--allow-absence-writes'],
      ['auth', 'logout', '--no-keep-alive'],
      ['auth', 'import'],
      ['auth', 'import', ''],
      ['auth', 'import', 'a', 'b'],
      ['auth', 'status', 'x'],
      ['auth', 'status', '--allow-account-change'],
      // No `--` case: `bun cli.ts -- ...` drops the first `--` before cli.ts sees it, while the
      // released Bun binary passes it on, as the Rust binary reads it (main.rs tests it).
      ['auth', 'bogus'],
      ['auth', '', 'x'],
    ].map((args) => ({ cli: args })),
  },
  { name: 'reads', steps: [{ seed: true }, { serve: READS }, { serve: READS }] },
  { name: 'reads-odd', steps: [{ seed: true }, { upstream: { odd: true } }, { serve: READS }] },
  { name: 'reads-signed-out', steps: [{ serve: READS }] },
  { name: 'reads-invalid', steps: [{ seed: true }, { serve: INVALID }] },
  { name: 'siblings', steps: [{ seed: true }, { serve: SIBLING_READS }, { serve: SIBLING_READS }] },
  {
    name: 'siblings-refused',
    steps: [
      { seed: true },
      { upstream: { ignoreSwitch: true } },
      { serve: [['inna_get_overview', { studentKey: '5' }]] },
      {
        upstream: {
          ignoreSwitch: false,
          switchLocation: 'https://example.invalid/Components/Students/Students.html',
        },
      },
      { serve: [['inna_get_overview', { studentKey: '5' }]] },
      { upstream: { switchLocation: 'http://nam.inna.is/other' } },
      { serve: [['inna_get_overview', { studentKey: '5' }]] },
      {
        upstream: {
          switchLocation: 'http://nam.inna.is/Components/Students/Students.html',
          planted: { '/auth/system': { status: 429, headers: { 'retry-after': '120' } } },
        },
      },
      {
        serve: [
          ['inna_get_overview', { studentKey: '5' }],
          ['inna_get_overview', {}],
        ],
      },
    ],
  },
  { name: 'failures', steps: [{ seed: true }, ...FAILURES] },
  {
    name: 'session-commands',
    env: { INNA_SESSION_FILE: '<home>/legacy/session.json' },
    steps: [
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'logout'] },
      { legacy: 'legacy/session.json' },
      { cli: ['auth', 'status'] },
      { serve: [['inna_session_status', {}]] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'status'] },
      { file: 'legacy/session.json', text: 'not a session' },
      { cli: ['auth', 'migrate'] },
      { legacy: 'legacy/session.json' },
      { cli: ['auth', 'logout'] },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { legacy: 'legacy/session.json' },
      { cli: ['auth', 'status'] },
      { seed: true },
      { cli: ['auth', 'status'] },
      { upstream: { planted: { '/api/UserData/GetLoggedInUser': { status: 401 } } } },
      { cli: ['auth', 'status'] },
    ],
  },
  {
    name: 'session-legacy-unreadable',
    env: { INNA_SESSION_FILE: '<home>/legacy/session.json' },
    steps: [
      { file: 'legacy/session.json', text: '{"version":3}' },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { file: 'legacy/session.json', text: LEGACY, permissions: 0o644 },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'migrate'] },
      { cli: ['auth', 'logout'] },
    ],
  },
  {
    name: 'session-relative-path',
    env: { INNA_SESSION_FILE: 'legacy/session.json' },
    steps: [
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'import', '<home>/cookies.json'] },
      { cli: ['auth', 'logout'] },
    ],
  },
  {
    name: 'import-refusals',
    steps: [
      { file: 'cookies.json', text: COOKIES },
      { cli: ['auth', 'import', 'cookies.json'] },
      { cli: ['auth', 'import', '<home>/missing.json'] },
      { cli: ['auth', 'import', '<home>'] },
      ...[
        ['broken', '{'],
        ['empty', '[]'],
        ['shape', '[{"name":"SESSION"}]'],
        ['object', '{"cookies":[]}'],
        ['domain', cookieFile([{ name: 'SESSION', value: 'v', domain: 'inna.is' }])],
        [
          'path',
          cookieFile([{ name: 'XSRF-TOKEN', value: 'v', domain: 'nam.inna.is', path: '/api' }]),
        ],
        [
          'others',
          cookieFile([{ name: 'other', value: 'v', domain: 'example.invalid', path: '/x' }]),
        ],
        ['bom', `\ufeff${COOKIES}`],
        // lint: defaults, not a non-null assertion; every pair has both.
      ].flatMap(([name = '', text = '']) => [
        { file: `${name}.json`, text },
        { cli: ['auth', 'import', `<home>/${name}.json`] },
      ]),
      { file: 'open.json', text: COOKIES, permissions: 0o644 },
      { cli: ['auth', 'import', '<home>/open.json'] },
      { cli: ['auth', 'status'] },
    ],
  },
  {
    name: 'import-accounts',
    steps: [
      { seed: true },
      { serve: [['inna_get_overview', { studentKey: '5' }]] },
      { upstream: { selected: '5' } },
      { seed: true },
      { upstream: { selected: '9' } },
      { seed: true },
      { upstream: { selected: '5' } },
      { seed: true, args: ['--allow-account-change'] },
      { cli: ['auth', 'status'] },
      { upstream: { selected: '1' } },
      { seed: true },
      {
        upstream: {
          planted: {
            '/api/UserData/GetLoggedInUser': { status: 429, headers: { 'retry-after': '120' } },
          },
        },
      },
      { seed: true, args: ['--allow-account-change'] },
      { upstream: { planted: {} } },
      { seed: true, args: ['--allow-account-change'] },
      { clock: NOW + 3_600_000 },
      { seed: true, args: ['--allow-account-change'] },
      { serve: [['inna_list_students', {}]] },
    ],
  },
  {
    // The phone prompt read from piped input, up to the first request: the fake Inna answers the
    // sign-in's first host with 404, which both sides report the same way.
    name: 'login-input',
    steps: [
      ...[
        '',
        '\n',
        ' 5550000 ',
        '123\r5550000\n',
        '555 0000\n',
        '5550000\u0000\n',
        '\ufeff5550000\u00a0\n',
        '５５５００００\n',
        '5550000\n',
      ].map((stdin) => ({ cli: ['auth', 'login'], stdin })),
      { cli: ['auth', 'login'] },
      { cli: ['auth', 'login', '--allow-account-change'], stdin: '5550000\r\n' },
      { seed: true },
      { cli: ['auth', 'login'], stdin: '5550000' },
      { cli: ['auth', 'status'] },
    ],
  },
  {
    // Google sign-in up to the browser: every case names a missing browser, so neither side can
    // open a real one.
    name: 'google-refusals',
    env: { INNA_BROWSER: '<home>/missing-from-environment' },
    steps: [
      ...[
        ['--timeout', '0'],
        ['--timeout=1.5'],
        ['--timeout=', '--browser', '<home>/missing'],
        ['--timeout', '0x10'],
        ['--timeout', ' 7 '],
        ['--browser', '<home>/missing'],
        ['--browser', '<home>'],
        [],
        ['--allow-account-change'],
      ].map((args) => ({ cli: ['auth', 'login', '--google', ...args] })),
      { file: 'not-executable', text: '#!/bin/sh\n', permissions: 0o600 },
      { cli: ['auth', 'login', '--google', '--browser', '<home>/not-executable'] },
    ],
  },
  {
    name: 'absence-writes',
    steps: [
      { seed: true },
      { upstream: clear() },
      {
        serve: [
          ...ABSENCE_INVALID,
          ['inna_prepare_absence', sick('2040-01-01')],
          ['inna_prepare_absence', sick('2040-01-03')],
          submit(),
          ['inna_prepare_absence', sick('2040-01-02')],
          ['inna_absence_status', {}],
          submit(),
          submit(),
          submit('123e4567-e89b-12d3-a456-426614174000'),
          ['inna_absence_status', {}],
          // A sibling's leave: prepared for that student, then submitted to it after a switch away.
          ['inna_prepare_absence', leave('2040-02-01', '2040-02-03', { studentKey: '5' })],
          ['inna_get_overview', {}],
          submit(),
          ['inna_absence_status', {}],
        ],
        args: WRITES,
      },
      // A preview past its ten minutes.
      { serve: [['inna_prepare_absence', leave('2040-03-01', '2040-03-01')]], args: WRITES },
      { clock: NOW + 600_000 },
      { serve: [submit(), ['inna_absence_status', {}]], args: WRITES },
      // Tomorrow's sick day, where Inna allows it.
      { upstream: { odd: true } },
      { upstream: clear() },
      { serve: [['inna_prepare_absence', sick('2040-01-03')]], args: WRITES },
    ],
  },
  {
    // Registered absences and leave applications that overlap, an unrecognized date after a
    // match and before one, and failed reads.
    name: 'absence-history',
    steps: [
      { seed: true },
      {
        serve: [
          ['inna_prepare_absence', leave('2040-01-02', '2040-01-05')],
          ['inna_prepare_absence', sick('2040-01-02')],
        ],
        args: WRITES,
      },
      {
        upstream: {
          planted: {
            '/api/RegisterAbsence/GetStudentRegisteredAbsences': { status: 200, body: '[]' },
          },
        },
      },
      {
        serve: [
          ['inna_prepare_absence', leave('2040-01-02', '2040-01-05')],
          ['inna_prepare_absence', leave('2040-01-03', '2040-01-05')],
        ],
        args: WRITES,
      },
      {
        upstream: {
          planted: {
            '/api/RegisterAbsence/GetStudentRegisteredAbsences': {
              status: 200,
              body: JSON.stringify([
                { id: 1, date: '01.03.2040', statusCode: 0, allDay: '1', classes: [] },
                { id: 2, date: '31.02.2040', statusCode: 0, allDay: '1', classes: [] },
              ]),
            },
          },
        },
      },
      {
        serve: [
          ['inna_prepare_absence', leave('2040-01-03', '2040-01-05')],
          ['inna_prepare_absence', leave('2040-03-01', '2040-03-02')],
        ],
        args: WRITES,
      },
      { upstream: { planted: { '/api/RegisterAbsence/GetRegisterAbsences': { status: 500 } } } },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      {
        upstream: {
          planted: { '/api/UserData/GetLoggedInUser': { status: 200, body: '{"x":1}' } },
        },
      },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
    ],
  },
  {
    // A write whose outcome is unknown blocks every later preview and is never sent again, also
    // by the other side and after a logout.
    name: 'absence-uncertain',
    steps: [
      { seed: true },
      { upstream: clear({ status: 500 }) },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      { serve: [submit(), ['inna_absence_status', {}]], args: WRITES, swap: true },
      {
        serve: [
          submit(),
          ['inna_prepare_absence', leave('2040-02-01', '2040-02-01', { studentKey: '5' })],
        ],
        args: WRITES,
      },
      { cli: ['auth', 'logout'] },
      { seed: true },
      {
        serve: [
          ['inna_absence_status', {}],
          ['inna_prepare_absence', sick('2040-01-02')],
        ],
        args: WRITES,
        swap: true,
      },
    ],
  },
  {
    // Each side submits the preview the other prepared, and each returns the other's result.
    name: 'absence-interop',
    steps: [
      { seed: true },
      { upstream: clear() },
      {
        serve: [['inna_prepare_absence', leave('2040-02-01', '2040-02-02', { studentKey: '5' })]],
        args: WRITES,
        swap: true,
      },
      { serve: [['inna_absence_status', {}], submit(), ['inna_absence_status', {}]], args: WRITES },
      {
        serve: [
          submit(),
          ['inna_absence_status', {}],
          ['inna_prepare_absence', leave('2040-02-01', '2040-02-02')],
        ],
        args: WRITES,
        swap: true,
      },
    ],
  },
  {
    // An answer without a positive ID leaves the outcome unknown on either side.
    name: 'absence-interop-unknown',
    steps: [
      { seed: true },
      { upstream: clear({ status: 200, body: '{"id":0}' }) },
      {
        serve: [['inna_prepare_absence', leave('2040-02-01', '2040-02-02', { studentKey: '5' })]],
        args: WRITES,
        swap: true,
      },
      { serve: [submit(), ['inna_absence_status', {}]], args: WRITES },
      { serve: [submit()], args: WRITES, swap: true },
    ],
  },
  {
    // Both refinements failing, alone and with field, key and length issues; the reason limit
    // counts code points.
    name: 'absence-refinements',
    steps: [
      {
        serve: [
          ['inna_prepare_absence', sick('2040-01-03', { dateTo: '2040-01-02' })],
          ['inna_prepare_absence', sick('2040-01-03', { dateTo: '2040-01-02', reason: ' ' })],
          ['inna_prepare_absence', sick('2040-01-03', { dateTo: '2040-01-02', q: 1 })],
          ['inna_prepare_absence', sick('2040-01-03', { dateTo: '2040-01-02', studentKey: '5x' })],
          ['inna_prepare_absence', leave('2040-01-03', '2040-01-02', { reason: 'x'.repeat(2001) })],
          [
            'inna_prepare_absence',
            sick('2040-01-03', {
              dateTo: '2040-01-02',
              reason: String.fromCodePoint(0x1f600).repeat(2000),
            }),
          ],
          [
            'inna_prepare_absence',
            sick('2040-01-03', {
              dateTo: '2040-01-02',
              reason: String.fromCodePoint(0x1f600).repeat(2001),
            }),
          ],
        ],
        args: WRITES,
      },
    ],
  },
  {
    // A 429 on the write: the outcome is unknown, the pause is saved and honoured without a
    // request, and the write is never sent again.
    name: 'absence-429-post',
    steps: [
      { seed: true },
      { upstream: clear({ status: 429, headers: { 'retry-after': '120' }, body: '' }) },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      {
        serve: [
          submit(),
          ['inna_absence_status', {}],
          submit(),
          ['inna_prepare_absence', sick('2040-01-02')],
        ],
        args: WRITES,
      },
      { clock: NOW + 120_001 },
      { upstream: clear() },
      {
        serve: [
          ['inna_absence_status', {}],
          submit(),
          ['inna_prepare_absence', sick('2040-01-02')],
        ],
        args: WRITES,
      },
    ],
  },
  {
    // A 429 on prepare's history read writes no preview, and the pause holds until it ends.
    name: 'absence-429-prepare',
    steps: [
      { seed: true },
      {
        upstream: {
          planted: {
            ...clear().planted,
            '/api/RegisterAbsence/GetLeaves': {
              status: 429,
              headers: { 'retry-after': '120' },
              body: '',
            },
          },
        },
      },
      { serve: [['inna_prepare_absence', leave('2040-02-01', '2040-02-02')]], args: WRITES },
      { upstream: clear() },
      {
        serve: [
          ['inna_prepare_absence', leave('2040-02-01', '2040-02-02')],
          ['inna_absence_status', {}],
        ],
        args: WRITES,
      },
      { clock: NOW + 120_001 },
      { serve: [['inna_prepare_absence', leave('2040-02-01', '2040-02-02')]], args: WRITES },
    ],
  },
  {
    // A 429 on submit's re-check sends no write and keeps the preview, which is submitted once the
    // pause ends.
    name: 'absence-429-submit-checks',
    steps: [
      { seed: true },
      { upstream: clear() },
      { serve: [['inna_prepare_absence', leave('2040-02-01', '2040-02-02')]], args: WRITES },
      {
        upstream: {
          planted: {
            ...clear().planted,
            '/api/RegisterAbsence/GetLeaves': {
              status: 429,
              headers: { 'retry-after': '60' },
              body: '',
            },
          },
        },
      },
      { serve: [submit()], args: WRITES },
      { upstream: clear() },
      { serve: [submit()], args: WRITES },
      { clock: NOW + 60_001 },
      { serve: [submit(), ['inna_absence_status', {}]], args: WRITES },
    ],
  },
  {
    // The host cancels a submit while its overlap check waits on Inna: nothing is sent, and the
    // preview stays prepared.
    name: 'absence-cancel-submit',
    steps: [
      { seed: true },
      { upstream: clear() },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      { upstream: { delays: { '/api/RegisterAbsence/GetLeaves': 1500 } } },
      {
        cancel: submit(),
        by: 'host',
        when: '/api/RegisterAbsence/GetLeaves',
        writes: 0,
        args: WRITES,
      },
      { upstream: { delays: {} } },
      { serve: [['inna_absence_status', {}]], args: WRITES },
    ],
  },
  {
    // The host cancels a prepare while its history read waits on Inna: no preview is written.
    name: 'absence-cancel-prepare',
    steps: [
      { seed: true },
      { upstream: { ...clear(), delays: { '/api/RegisterAbsence/GetLeaves': 1500 } } },
      {
        cancel: ['inna_prepare_absence', sick('2040-01-02')],
        by: 'host',
        when: '/api/RegisterAbsence/GetLeaves',
        writes: 0,
        args: WRITES,
      },
      { upstream: { delays: {} } },
      { serve: [['inna_absence_status', {}]], args: WRITES },
    ],
  },
  {
    // The host cancels a submit while Inna holds its write: the outcome is unknown.
    name: 'absence-cancel-post',
    steps: [
      { seed: true },
      { upstream: clear() },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      { upstream: { delays: { '/api/RegisterAbsence/AddNewLeave': 1500 } } },
      {
        cancel: submit(),
        by: 'host',
        when: '/api/RegisterAbsence/AddNewLeave',
        writes: 1,
        args: WRITES,
      },
      { upstream: { delays: {} } },
      { serve: [['inna_absence_status', {}], submit()], args: WRITES },
    ],
  },
  {
    // The server's stdin ends while a submit's overlap check waits on Inna: the call is aborted at
    // once, so nothing is sent.
    name: 'absence-close-submit',
    steps: [
      { seed: true },
      { upstream: clear() },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      { upstream: { delays: { '/api/RegisterAbsence/GetLeaves': 1500 } } },
      {
        cancel: submit(),
        by: 'stdin',
        when: '/api/RegisterAbsence/GetLeaves',
        writes: 0,
        args: WRITES,
      },
      { upstream: { delays: {} } },
      { serve: [['inna_absence_status', {}]], args: WRITES },
    ],
  },
  {
    // SIGINT while a submit's overlap check waits on Inna: nothing is sent.
    name: 'absence-sigint-submit',
    steps: [
      { seed: true },
      { upstream: clear() },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      { upstream: { delays: { '/api/RegisterAbsence/GetLeaves': 1500 } } },
      {
        cancel: submit(),
        by: 'SIGINT',
        when: '/api/RegisterAbsence/GetLeaves',
        writes: 0,
        args: WRITES,
      },
      { upstream: { delays: {} } },
      { serve: [['inna_absence_status', {}]], args: WRITES },
    ],
  },
  {
    // SIGTERM while a submit's overlap check waits on Inna: nothing is sent.
    name: 'absence-sigterm-submit',
    steps: [
      { seed: true },
      { upstream: clear() },
      { serve: [['inna_prepare_absence', sick('2040-01-02')]], args: WRITES },
      { upstream: { delays: { '/api/RegisterAbsence/GetLeaves': 1500 } } },
      {
        cancel: submit(),
        by: 'SIGTERM',
        when: '/api/RegisterAbsence/GetLeaves',
        writes: 0,
        args: WRITES,
      },
      { upstream: { delays: {} } },
      { serve: [['inna_absence_status', {}]], args: WRITES },
    ],
  },
  {
    // A store that passes the startup check but holds a damaged record or marker: `auth login`
    // refuses it before it asks for the phone number.
    name: 'login-store-unusable',
    steps: [
      { seed: true },
      { file: storeFile, text: 'x' },
      { cli: ['auth', 'login'], stdin: '5550000\n', absent: 'phone number' },
    ],
  },
  {
    name: 'login-marker-unusable',
    steps: [
      { seed: true },
      { file: `${storeFile}.marker`, text: 'x' },
      { cli: ['auth', 'login'], stdin: '5550000\n', absent: 'phone number' },
    ],
  },
  {
    name: 'store-file-open',
    steps: [
      { file: storeFile, text: 'x', permissions: 0o644 },
      { cli: ['auth', 'status'] },
      { cli: ['auth', 'bogus'] },
    ],
  },
  {
    name: 'store-folder-open',
    steps: [
      { file: storeFile, text: 'x' },
      { directory: '.config/inna-mcp', permissions: 0o755 },
      { cli: ['serve'] },
      { cli: ['auth', 'login'] },
    ],
  },
];
// ---- end of parity.ts ----

setSystemTime();

/** The environment of a run in `home`: parity.ts's, with the proxy as the test origin. */
function environment(home: string, extra: Record<string, string> = {}) {
  return {
    PATH: process.env.PATH ?? '/usr/bin:/bin',
    HOME: home,
    TMPDIR: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_DATA_HOME: join(home, '.local/share'),
    XDG_STATE_HOME: join(home, '.local/state'),
    XDG_CACHE_HOME: join(home, '.cache'),
    BUN_RUNTIME_TRANSPILER_CACHE_PATH: '0',
    FAMILY_MCP_STORE_TEST_SEAM: '1',
    INNA_TEST_ORIGIN: origin,
    INNA_TEST_NOW: clock,
    ...extra,
  };
}

/** Values no output may hold: what fake-inna.ts marks as never returned, and every cookie value. */
const LEAKS = [
  'DO-NOT-RETURN',
  '9999999999',
  'synthetic-session',
  'synthetic-xsrf',
  'synthetic-rotated',
  'synthetic-switched',
  // The contract set's own: the access decoy, the sign-in cookies, and the access list's id.
  '8888888',
  'synthetic-saved',
  'synthetic-school',
  'synthetic-identity-cookie',
  'DO-NOT-SAVE',
];

function leaks(at: string, text: string): void {
  for (const leak of LEAKS)
    if (text.includes(leak)) throw new RangeError(`${at}: an output holds ${leak}.`);
}

/** A prepare's answer: its operation ID. */
const preparedAnswer = z.object({
  result: z.object({ structuredContent: z.object({ operationId: z.string() }) }),
});

/**
 * Per home: `<home>` for the scratch home, `<operation-N>` for the Nth operation ID a prepare
 * returned, an ISO time from 2020 up to 2039 as `<wall-clock>` (a cookie jar's creation and last
 * access times are the wall clock's, not the scenario clock's, which reads 2040; no scenario value
 * falls in that span, the fake's epoch values being 1970 or 2040 and later), and, in a scenario
 * that plants a 401, `synthetic-rotated-<rotation>` for each rotated cookie value (the renewal the
 * binary adds after a 401 shifts the count; parity.ts normalized it the same way).
 */
class Names {
  readonly operations: string[] = [];

  constructor(
    private readonly home: string,
    private readonly rotation: boolean,
  ) {}

  /** Learns the operation ID of a prepare's answer. */
  learn(answer: string): void {
    const prepared = preparedAnswer.safeParse(JSON.parse(answer));

    if (prepared.success) {
      const id = prepared.data.result.structuredContent.operationId;

      if (!this.operations.includes(id)) this.operations.push(id);
    }
  }

  fresh(text: string): string {
    let named = text.replaceAll(realpathSync(this.home), '<home>').replaceAll(this.home, '<home>');

    for (const [index, id] of this.operations.entries())
      named = named.replaceAll(id, `<operation-${index + 1}>`);

    named = named.replaceAll(/\b20[23]\d-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z/g, (iso) =>
      iso < '2039' ? '<wall-clock>' : iso,
    );

    return this.rotation
      ? named.replaceAll(/synthetic-rotated-\d+/g, 'synthetic-rotated-<rotation>')
      : named;
  }
}

const toolArguments = z.record(z.string(), z.unknown());

type Arguments = z.infer<typeof toolArguments>;

/**
 * A call's arguments (as JSON text) with `<operation>` naming the last prepared operation, as in
 * parity.ts.
 */
function placed(text: string, names: Names): Arguments {
  return toolArguments.parse(
    JSON.parse(text.replaceAll('<operation>', names.operations.at(-1) ?? '<operation>')),
  );
}

/** The exchanges since `start`, their requests named like outputs; responses are served raw. */
function since(start: number, names: Names): Exchange[] {
  const named: Exchange[] = [];

  for (const exchange of exchanges.slice(start)) {
    assert.ok(exchange.response, `${exchange.request.url} was never answered`);

    const { method, url, headers, body } = exchange.request;

    const request = {
      method,
      url: names.fresh(url),
      headers: Object.fromEntries(
        Object.entries(headers).map(([name, value]) => [
          name,
          value === null ? null : names.fresh(value),
        ]),
      ),
      body: names.fresh(body),
    };

    const kept: Exchange = { request, response: exchange.response };

    if (exchange.recovery) kept.recovery = exchange.recovery;

    named.push(kept);
  }

  return named;
}

function command(args: string[]): string[] {
  return [process.execPath, '--preload', rewrite, cli, ...args];
}

async function runCli(
  home: string,
  names: Names,
  env: Record<string, string>,
  step: { cli: string[]; stdin?: string; absent?: string },
) {
  const start = exchanges.length;
  const place = (text: string) => text.replaceAll('<home>', home);

  const child = Bun.spawn(command(step.cli.map(place)), {
    cwd: home,
    env: environment(home, env),
    stdin: step.stdin === undefined ? 'ignore' : new Blob([step.stdin]),
    stdout: 'pipe',
    stderr: 'pipe',
  });

  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);

  leaks(home, stdout + stderr);

  if (step.absent !== undefined && (stdout + stderr).includes(step.absent))
    throw new RangeError(`${home}: printed ${JSON.stringify(step.absent)}`);

  return {
    ...step,
    code,
    stdout: names.fresh(stdout),
    stderr: names.fresh(stderr),
    exchanges: since(start, names),
  };
}

/** The protocol version and client the replay sends too. */
const INITIALIZE = {
  protocolVersion: '2025-11-25',
  capabilities: {},
  clientInfo: { name: 'inna-parity', version: '1.0.0' },
};

type Params =
  | typeof INITIALIZE
  | { name: string; arguments: Arguments }
  | { requestId: number; reason: string }
  | Record<string, never>;

type Outgoing = { id?: number; method: string; params?: Params };

/** A message the server wrote: an answer carries an id, a notification none. */
const incomingMessage = z.looseObject({
  jsonrpc: z.literal('2.0'),
  id: z.union([z.number(), z.string(), z.null()]).optional(),
});

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

/** `inna-mcp serve` over raw JSON-RPC, as one MCP client for the server's life. */
class Server {
  private readonly child;

  private readonly incoming;

  private id = 0;

  constructor(home: string, env: Record<string, string>, args: string[]) {
    this.child = Bun.spawn(command(['serve', ...args]), {
      cwd: home,
      env: environment(home, env),
      stdin: 'pipe',
      stdout: 'pipe',
      stderr: 'pipe',
    });
    this.incoming = lines(this.child.stdout);
  }

  async send(message: Outgoing): Promise<void> {
    await this.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', ...message })}\n`);
    await this.child.stdin.flush();
  }

  /** Sends a request and returns its id. */
  async begin(method: string, params: Params): Promise<number> {
    const sent = this.id++;
    await this.send({ id: sent, method, params });

    return sent;
  }

  /** The answer to `id`, with the notifications before it. */
  async answer(id: number): Promise<{ notifications: string[]; answer: string }> {
    const notifications: string[] = [];

    for (;;) {
      const next = await this.incoming.next();

      if (next.done === true) throw new RangeError(`The server closed before answering ${id}.`);

      const message = incomingMessage.parse(JSON.parse(next.value));

      if (message.id === undefined) {
        notifications.push(next.value);
        continue;
      }

      if (message.id !== id)
        throw new RangeError(`An answer to ${JSON.stringify(message.id)}, not ${id}.`);

      return { notifications, answer: next.value };
    }
  }

  async request(method: string, params: Params) {
    return this.answer(await this.begin(method, params));
  }

  async initialize(): Promise<string> {
    const { answer } = await this.request('initialize', INITIALIZE);
    await this.send({ method: 'notifications/initialized' });

    return answer;
  }

  async close(): Promise<{ code: number | null; stderr: string }> {
    await this.child.stdin.end();

    const [stderr, code] = await Promise.all([
      new Response(this.child.stderr).text(),
      this.child.exited,
    ]);

    return { code, stderr };
  }

  kill(signal: 'SIGINT' | 'SIGTERM' | 'SIGKILL'): void {
    this.child.kill(signal);
  }

  async exited(patience: number): Promise<number | 'running'> {
    return Promise.race([this.child.exited, Bun.sleep(patience).then(() => 'running' as const)]);
  }

  get signal(): string | null {
    return this.child.signalCode;
  }
}

type Result = {
  call: [string, unknown];
  notifications?: unknown[];
  answer: string;
  exchanges: Exchange[];
};

type FrozenServe = {
  serve: true;
  args?: string[];
  swap?: true;
  initialize?: string;
  list?: string;
  results?: Result[];
  code?: number | null;
  stderr?: string;
};

async function runServe(
  home: string,
  names: Names,
  env: Record<string, string>,
  step: { serve: [string, unknown][]; args?: string[]; surface?: boolean; swap?: boolean },
): Promise<FrozenServe> {
  const server = new Server(home, env, step.args ?? []);
  const initialize = await server.initialize();
  const list = step.surface === true ? (await server.request('tools/list', {})).answer : undefined;
  const results: Result[] = [];

  for (const [name, args] of step.serve) {
    const start = exchanges.length;

    const { notifications, answer } = await server.request('tools/call', {
      name,
      arguments: placed(JSON.stringify(args), names),
    });

    leaks(home, answer + notifications.join(''));
    names.learn(answer);
    results.push(
      Object.assign(
        { call: [name, args] satisfies [string, unknown] },
        notifications.length
          ? {
              notifications: notifications.map((line) =>
                z.unknown().parse(JSON.parse(names.fresh(line))),
              ),
            }
          : undefined,
        { answer: names.fresh(answer), exchanges: since(start, names) },
      ),
    );
  }

  const { code, stderr } = await server.close();
  leaks(home, stderr);

  // Built in this order, the fixture's key order.
  const frozen: FrozenServe = { serve: true };

  if (step.args) frozen.args = step.args;

  if (step.swap === true) frozen.swap = true;

  if (list !== undefined) {
    frozen.initialize = initialize;
    frozen.list = list;
  }

  frozen.results = results;
  frozen.code = code;
  frozen.stderr = names.fresh(stderr);

  return frozen;
}

/** Resolves once the proxy has seen a request to `path` after the first `from` exchanges. */
async function seenPath(path: string, from: number): Promise<void> {
  const deadline = Date.now() + 20_000;

  while (
    !exchanges.slice(from).some((exchange) => new URL(exchange.request.url).pathname === path)
  ) {
    if (Date.now() > deadline) throw new Error(`Inna never saw ${path}`);

    await Bun.sleep(10);
  }
}

type Cancel = {
  cancel: [string, unknown];
  by: 'host' | 'stdin' | 'SIGINT' | 'SIGTERM';
  when: string;
  writes: number;
  args?: string[];
};

type Outcome =
  | { call: string; code: number | null; stderr: string }
  | { exited: number | 'running'; signal: string | null };

/**
 * parity.ts's cancelled call: by the host (`notifications/cancelled`, as the SDK client's abort
 * sends it), by the end of the server's standard input, or by a signal, once the request to
 * `when` has reached Inna, which holds it; then long enough for that request to be answered and
 * anything sent after it to arrive. The host's cancel keeps whether the server still answered the
 * call; the others how the server exited (parity.ts compared no output of them).
 */
async function runCancel(home: string, names: Names, env: Record<string, string>, step: Cancel) {
  const settle = (current.state.delays[step.when] ?? 0) + 1000;
  const start = exchanges.length;
  const [name, input] = step.cancel;
  const server = new Server(home, env, step.args ?? []);
  await server.initialize();

  const id = await server.begin('tools/call', {
    name,
    arguments: placed(JSON.stringify(input), names),
  });

  await seenPath(step.when, start);
  let outcome: Outcome;

  if (step.by === 'host') {
    await server.send({
      method: 'notifications/cancelled',
      params: { requestId: id, reason: 'AbortError' },
    });

    await Bun.sleep(settle);
    // A ping answered before any answer to the call shows the call was dropped.
    const ping = await server.begin('ping', {});
    let call = 'no answer';

    try {
      await server.answer(ping);
    } catch (error) {
      call = error instanceof Error ? error.message : String(error);
    }

    const closed = await server.close();
    leaks(home, closed.stderr);
    outcome = { call, code: closed.code, stderr: names.fresh(closed.stderr) };
  } else {
    if (step.by === 'stdin') await server.close();
    else server.kill(step.by);

    const exited = await server.exited(settle + 5000);

    if (exited === 'running') server.kill('SIGKILL');
    else await Bun.sleep(settle);

    outcome = { exited, signal: server.signal };

    if (step.by !== 'stdin') await server.close();
  }

  const sent = since(start, names);

  const writes = sent.filter((exchange) =>
    exchange.request.url.endsWith('/api/RegisterAbsence/AddNewLeave'),
  ).length;

  assert.equal(writes, step.writes, `${home}: ${writes} absence writes, not ${step.writes}`);

  return { ...step, delays: current.state.delays, outcome, exchanges: sent };
}

/** Every path under `home` (a directory ends in `/`) with its permission bits, sorted. */
function files(home: string, names: Names): string[] {
  return readdirSync(home, { recursive: true, withFileTypes: true })
    .map((entry) => {
      const raw = relative(home, join(entry.parentPath, entry.name));
      const info = lstatSync(join(home, raw));
      const mode = (info.mode & 0o777).toString(8);

      return info.isDirectory() ? `${names.fresh(raw)}/ ${mode}` : `${names.fresh(raw)} ${mode}`;
    })
    .toSorted();
}

/** The text of each absence record (`<legacy>.absence.json`) under `home`, by path. */
function journals(home: string, names: Names) {
  return Object.fromEntries(
    readdirSync(home, { recursive: true, withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.endsWith('.absence.json'))
      .map((entry) => {
        const path = join(entry.parentPath, entry.name);

        return [names.fresh(relative(home, path)), names.fresh(readFileSync(path, 'utf8'))];
      }),
  );
}

type PlantEntry = { path: string; mode: number; hex?: string };

/** Every file and folder under `home` with its mode, and each file's bytes in hex, for a `swap` plant. */
function snapshot(home: string): PlantEntry[] {
  return readdirSync(home, { recursive: true, withFileTypes: true })
    .map((entry): PlantEntry => {
      const path = relative(home, join(entry.parentPath, entry.name));
      const full = join(home, path);
      const mode = statSync(full).mode & 0o777;

      if (entry.isDirectory()) return { path, mode };

      const bytes = readFileSync(full);
      assert.ok(!bytes.includes(home), `${path} names the home`);

      return { path, mode, hex: bytes.toString('hex') };
    })
    .toSorted((first, second) =>
      first.path < second.path ? -1 : first.path > second.path ? 1 : 0,
    );
}

/** Run `work` with `home`'s environment in this process (the store paths follow it). */
async function inHome<T>(home: string, work: () => Promise<T>): Promise<T> {
  const before = { ...process.env };
  Object.assign(process.env, environment(home));

  try {
    return await work();
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in before)) delete process.env[key];
    Object.assign(process.env, before);
  }
}

type Stored = { store: 'none' } | { record: string | null } | { error: string };

/** What the store holds: no store, the decrypted record text, or the error code. */
async function stored(home: string, names: Names): Promise<Stored> {
  return inHome(home, async () => {
    const options = {
      path: defaultSecretRecordPath('inna-mcp'),
      server: 'inna-mcp',
      profile: 'default',
      purpose: 'session',
      schema: 1,
      maxBytes: RECORD_MAX_BYTES,
      keys: defaultKeyProvider({ server: 'inna-mcp', profile: 'default' }),
    };

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

function writeStep(home: string, step: { file: string; text: string; permissions?: number }): void {
  const path = join(home, step.file);
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  writeFileSync(path, step.text, { mode: step.permissions ?? 0o600 });
  chmodSync(path, step.permissions ?? 0o600);
}

/** Whether a scenario plants a 401, where parity.ts named rotated cookies `<rotation>`. */
const plants401 = (scenario: Scenario) =>
  scenario.steps.some(
    (step) =>
      'upstream' in step &&
      Object.values(step.upstream.planted ?? {}).some((reply) => reply.status === 401),
  );

type Frozen = {
  id: string;
  env?: Record<string, string>;
  rotation?: true;
  steps: object[];
  requests: number;
  files: string[];
};

async function run(scenario: Scenario) {
  const home = join(scratch, scenario.name);
  mkdirSync(home, { recursive: true, mode: 0o700 });
  const rotation = plants401(scenario);
  const names = new Names(home, rotation);
  current = { state: fresh(), seen: [] };
  const start = exchanges.length;
  writeFileSync(clock, String(NOW));
  const place = (text: string) => text.replaceAll('<home>', home);

  const env = Object.fromEntries(
    Object.entries(scenario.env ?? {}).map(([name, value]) => [name, place(value)]),
  );

  const steps: object[] = [];
  const journal = () => ({ journal: journals(home, names) });

  for (const step of scenario.steps) {
    if ('cli' in step) steps.push({ ...(await runCli(home, names, env, step)), ...journal() });
    else if ('seed' in step) {
      writeStep(home, { file: 'cookies.json', text: COOKIES });
      steps.push({ file: 'cookies.json', text: COOKIES });
      const args = ['auth', 'import', '<home>/cookies.json', ...(step.args ?? [])];
      steps.push({ ...(await runCli(home, names, env, { cli: args })), ...journal() });
    } else if ('legacy' in step) {
      writeStep(home, { file: step.legacy, text: LEGACY });
      steps.push({ file: step.legacy, text: LEGACY });
    } else if ('upstream' in step) {
      Object.assign(current.state, step.upstream);
      steps.push(step);
    } else if ('clock' in step) {
      writeFileSync(clock, String(step.clock));
      steps.push(step);
    } else if ('serve' in step) {
      // A `swap` step's home as the TypeScript side had written it, planted by the replay.
      const plant = step.swap === true ? snapshot(home) : undefined;
      const operations = [...names.operations];
      const frozen = await runServe(home, names, env, step);

      steps.push(
        plant === undefined
          ? { ...frozen, ...journal() }
          : { plant, operations, ...frozen, ...journal() },
      );
    } else if ('cancel' in step)
      steps.push({ ...(await runCancel(home, names, env, step)), ...journal() });
    else if ('directory' in step) {
      chmodSync(join(home, step.directory), step.permissions);
      steps.push(step);
    } else {
      writeStep(home, step);
      steps.push(step);
    }
  }

  const frozen: Frozen = Object.assign(
    { steps, id: `inna/${set}/${scenario.name}` },
    scenario.env ? { env: scenario.env } : undefined,
    rotation ? { rotation: true as const } : undefined,
    { requests: exchanges.length - start, files: files(home, names) },
  );

  return { ...frozen, ...(await stored(home, names)) };
}

/**
 * Long runs of one character inside JSON strings become `\u0001<character>*<count>\u0001`; the
 * replay expands them. No frozen value holds a U+0001 (login-input's standard input holds a NUL,
 * so the sentinel is not Domino's NUL).
 */
function pack(text: string): string {
  if (text.includes('\\u0001')) throw new RangeError('A frozen value holds a U+0001.');

  return text.replaceAll(
    /(?<!\\)([^\\"])\1{1023,}/g,
    (sequence: string, unit: string) => `\\u0001${unit}*${sequence.length}\\u0001`,
  );
}

/** One step per line, so a fixture's diff names the step that changed. */
function layout(frozen: { steps: object[] }): string {
  const { steps, ...rest } = frozen;
  const head = JSON.stringify(rest);

  return `{"steps":[\n${steps.map((step) => JSON.stringify(step)).join(',\n')}\n],${head.slice(1)}\n`;
}

// ---- integration.test.ts (41d6f98): the contract cases' Inna ----
const request = {
  kind: 'sick',
  dateFrom: '2040-01-02',
  dateTo: '2040-01-02',
  reason: 'Synthetic reason',
} satisfies Parameters<InnaClient['prepareAbsence']>[0];

const student: User = {
  userId: 1,
  studentId: '2',
  schoolId: '3',
  studentName: 'Synthetic student',
  name: 'Synthetic guardian',
  schoolLong: 'Synthetic school',
  defaultTermId: '4',
  isGuardian: true,
  logInType: '2',
  olderThan18: false,
  registerAbsenceGuardian: '1',
  registerAbsenceUnder18: '0',
  registerAbsenceOver18: '1',
  registerAbsence: '1',
  student18RegisterAbsence: '1',
  registerLeave: '1',
  student18RegisterLeave: '1',
  registerIllnessTomorrow: '1',
};

const sibling: User = {
  ...student,
  userId: 5,
  studentId: '6',
  schoolId: '8',
  studentName: 'Synthetic sibling',
  schoolLong: 'Synthetic second school',
};

const SIBLING = '5';

// copy: `range` is the parity tables' own name here; the contract cases use `days`.

// Decoys for the access fields that must never reach a tool result or the session file.
const hidden = {
  kennitala: '9999999999',
  notandi_id: '8888888',
  url_login: 'https://nam.inna.is/auth/token?token=DO-NOT-RETURN',
};

function expectClean(text: string): void {
  for (const decoy of ['9999999999', '8888888', 'DO-NOT-RETURN', 'auth/token'])
    expect(text).not.toContain(decoy);
}

type AccessEntry = { system: string; status: string; skoli_id?: string; skoli_heiti: string };

class Provider {
  readonly calls: { url: URL; method: string; body: string | undefined }[] = [];
  posts = 0;
  failPost = false;
  redirect = false;
  rateLimit = false;
  ignoreSwitch = false;
  switchRateLimit = false;
  unauthorized = false;
  switchUnauthorized = false;
  rotation = 'synthetic-rotated';
  numeric = false;
  switchLocation = 'http://nam.inna.is/Components/Students/Students.html';
  switches = 0;
  user = student;
  selected = '1';
  order = ['1', SIBLING, '9'];
  contexts = new Map([
    ['1', student],
    [SIBLING, sibling],
  ]);

  // Live shape: digit strings, no id key. The last entry is another Inna application.
  entries = new Map<string, AccessEntry>([
    ['1', { system: '1', status: '1', skoli_id: '3', skoli_heiti: 'Synthetic school' }],
    [SIBLING, { system: '1', status: '2', skoli_id: '8', skoli_heiti: 'Synthetic second school' }],
    ['9', { system: '2', status: '1', skoli_id: '3', skoli_heiti: 'Synthetic school' }],
  ]);

  access() {
    return this.order.map((key) => {
      const entry = this.entries.get(key);
      const on = key === this.selected;

      return {
        ...hidden,
        ...entry,
        userId: this.numeric ? Number(key) : key,
        system: this.numeric ? Number(entry?.system) : entry?.system,
        loggedIn: this.numeric ? on : on ? '1' : '0',
        isStudent: '1',
        virkur: '1',
        adgangur: '1',
        nafn: this.contexts.get(key)?.studentName ?? 'Synthetic staff',
        title: 'Synthetic role',
        tegund: 'Synthetic kind',
        skoli_audk: 'SYN',
        showPersonalInfoConfirmationModal: false,
      };
    });
  }

  paths(from = 0): string[] {
    return this.calls.slice(from).map((call) => call.url.pathname);
  }

  private switchStudent(url: URL, headers: Headers, options: RequestInit): Response {
    expect(options.method).toBe('GET');
    expect(options.body).toBeUndefined();
    expect(headers.get('X-Requested-By')).toBeNull();
    expect(headers.get('X-XSRF-TOKEN')).toBeNull();
    expect(headers.get('Authorization')).toBeNull();
    this.switches += 1;

    if (this.switchRateLimit)
      return new Response(null, { status: 429, headers: { 'Retry-After': '120' } });

    if (this.switchUnauthorized) return new Response(null, { status: 401 });
    const key = this.order[Number(url.searchParams.get('i'))];
    const entry = key === undefined ? undefined : this.entries.get(key);

    expect([...url.searchParams.keys()]).toEqual(['i', 'system', 'status', 'user_id']);
    expect(url.searchParams.get('system')).toBe('1');
    expect(url.searchParams.get('system')).toBe(String(entry?.system));
    expect(url.searchParams.get('status')).toBe(String(entry?.status));
    expect(url.searchParams.get('user_id')).toBe(key ?? '');

    if (key !== undefined && !this.ignoreSwitch) {
      this.selected = key;
      this.user = this.contexts.get(key) ?? this.user;
    }

    return new Response('<html>Synthetic redirect</html>', {
      status: 303,
      headers: {
        Location: this.switchLocation,
        'Set-Cookie': `SESSION=synthetic-switched-${this.switches}; Path=/; Secure; HttpOnly`,
      },
    });
  }

  fetch = async (value: string, options: RequestInit): Promise<Response> => {
    const url = new URL(value);
    expect(url.origin).toBe('https://nam.inna.is');
    expect(options.redirect).toBe('manual');
    const headers = new Headers(options.headers);
    expect(headers.get('Cookie')).toContain('SESSION=synthetic-');
    this.calls.push({
      url,
      method: options.method ?? 'GET',
      body: z.string().optional().parse(options.body),
    });

    if (url.pathname === '/auth/system') return this.switchStudent(url, headers, options);

    if (url.pathname === '/Components/Students/Students.html')
      return new Response('<html>Synthetic application</html>', {
        headers: { 'Content-Type': 'text/html' },
      });
    expect(url.pathname.startsWith('/api/')).toBe(true);
    expect(headers.get('X-Requested-By')).toBe('XMLHttpRequest');
    expect(headers.get('X-XSRF-TOKEN')).toBe('synthetic-xsrf');

    if (this.redirect)
      return new Response(null, {
        status: 302,
        headers: { Location: 'https://example.invalid/credential-trap' },
      });

    if (this.rateLimit)
      return new Response(null, { status: 429, headers: { 'Retry-After': '60' } });

    if (this.unauthorized) return new Response(null, { status: 401 });

    if (url.pathname === '/api/UserData/GetLoggedInUser')
      return Response.json(
        {
          ...this.user,
          access: this.access(),
          studentIdNumber: 'DO-NOT-RETURN',
          privateToken: 'DO-NOT-RETURN',
        },
        { headers: { 'Set-Cookie': `SESSION=${this.rotation}; Path=/; Secure; HttpOnly` } },
      );

    if (url.pathname === '/api/RegisterAbsence/AddNewLeave') {
      this.posts += 1;
      expect(options.method).toBe('POST');

      if (this.failPost) throw new Error('Synthetic lost response');

      return Response.json({ id: 123 });
    }

    switch (url.pathname) {
      case '/api/StudentTerms/GetStudentTerms':
        return Response.json([{ termId: '4', termCode: 'Synthetic term' }]);
      case '/api/ModulesAndBooklist/GetModulesAndBooklist':
      case '/api/Homework/GetStudentHomework':
      case '/api/StudentGrades/GetStudentGrades':
      case '/api/RegisterAbsence/GetLeaves':
      case '/api/RegisterAbsence/GetStudentRegisteredAbsences':
        return Response.json([]);
      case '/api/Announcements/GetStudentAnnouncements':
        return Response.json([
          {
            announcementId: '12',
            date: '02.01.2040',
            title: 'Synthetic school-wide announcement',
            sender: 'Synthetic school',
            contentHtml: '<p>Synthetic notice</p>',
            hasOpened: false,
          },
        ]);
      case '/api/Timetable/GetTimetable':
        return Response.json([
          {
            start: '2040-01-02T10:00:00',
            end: '2040-01-02T11:00:00',
            titleShort: 'Synthetic lesson',
            allDay: false,
          },
          { start: '2040-01-02', end: '2040-01-03', titleShort: 'Synthetic event', allDay: true },
        ]);
      case '/api/GetAssignments/GetStudentAssignments':
        return Response.json([
          {
            assignmentId: '5',
            name: 'Synthetic assignment',
            module: 'Synthetic course',
            type: '0',
            assignedFullDate: '01.01.2040',
            handInFullDate: '03.01.2040',
            handedIn: 0,
            isOpen: 1,
            projectId: '6',
          },
        ]);
      case '/api/GetAssignments/GetAssignmentInfo':
        return Response.json({
          assignmentId: '5',
          name: 'Synthetic assignment',
          description: '<p>Instructions</p><script>discard()</script>',
          moduleName: 'Synthetic course',
          groupId: '7',
          groupName: '1',
          moduleTermId: '8',
          returnDate: '03.01.2040',
          type: 0,
          exam: 0,
          weight: '20',
          projectId: '6',
          groupReturnSize: 0,
        });
      case '/api/GetAssignments/Groups/7/StudentProjects':
        return Response.json({
          assignments: [
            {
              id: 5,
              name: 'Synthetic assessment',
              type: 0,
              weight: 20,
              grade: '8',
              returnDate: 1,
              assignDate: 1,
              handedIn: true,
            },
          ],
          categories: [],
        });
      case '/api/Attendance/GetAttendance':
        return Response.json({
          absencesTotal: [],
          leaveOfAbsencesTotal: [],
          attendanceTerm: { realAttendance: '90', attendance: '95' },
          dateFrom: '01.01.2040',
          dateTo: '01.05.2040',
          termName: 'Synthetic term',
          nrClassesTotal: 10,
          absencePointsTotal: '1',
          modules: [],
        });
      case '/api/Attachment/GetModuleFiles':
        return Response.json([
          {
            fileGroupId: '9',
            groupId: '7',
            fileGroup: 'Synthetic group',
            files: [
              {
                name: 'Synthetic file',
                fileId: '10',
                closed: false,
                link: 'https://example.invalid/do-not-fetch',
              },
            ],
          },
        ]);
      case '/api/Messages/GetReceivedMessages':
        return Response.json({
          count: 1,
          messages: [
            {
              messagesId: '11',
              table: 'A',
              title: 'Synthetic message',
              sender: 'Synthetic sender',
              date: '02.01.2040',
              status: 'S',
            },
          ],
        });
      case '/api/Messages/GetMessageDetails':
        return Response.json({
          title: 'Synthetic message',
          message: '<p>Hello &amp; welcome</p>',
          dateCreated: '02.01.2040',
          dateSent: '02.01.2040',
          sentTo: 'Synthetic recipient',
          type: 'A',
          attachmentList: [],
        });
      case '/api/RegisterAbsence/GetRegisterAbsences':
        return Response.json({
          todayAllowed: true,
          tomorrowAllowed: true,
          today: false,
          tomorrow: false,
          doctorsNote: '',
          comment: '',
        });
      default:
        throw new Error('Unimplemented synthetic endpoint');
    }
  };
}

// ---- end of integration.test.ts ----

// ---- login.test.ts (41d6f98): the sign-in cases' Inna ----
const token = `${Buffer.from('{"alg":"HS256","typ":"JWT"}').toString('base64url')}.${Buffer.from('{"sub":"synthetic-subject"}').toString('base64url')}.${'x'.repeat(32)}-`;

const bodySchema = z.object({
  returnUrl: z.string().optional(),
  userIdentifier: z.string().optional(),
  verificationProperties: z.string().optional(),
  data: z.string().optional(),
  session: z.object({ data: z.string(), isSuccess: z.boolean() }).optional(),
});

const session = (data: string, success = false, wait = 2000) => ({
  isSuccess: success,
  retryWaitTime: wait,
  retries: 10,
  nexusUrl: null,
  data,
  timeoutErrorMessage: 'Synthetic expired message',
  isFirstPoll: false,
  scriptId: null,
  sessionId: null,
  deviceLinkUrl: null,
});

const redirect = (url: string, cookies?: string) =>
  new Response(null, {
    status: 302,
    headers: cookies ? { Location: url, 'Set-Cookie': cookies } : { Location: url },
  });

function provider() {
  const calls: { url: URL; method: string }[] = [];
  let polls = 0;
  let terms = true;
  let access = [{ system: 1, user_id: 2, status: 3, is_access: true }];
  const selections: Record<string, string>[] = [];
  let trap = false;

  const fetcher = async (value: string, options: RequestInit): Promise<Response> => {
    const url = new URL(value);
    const headers = new Headers(options.headers);
    const method = options.method ?? 'GET';
    const body = bodySchema.parse(JSON.parse(z.string().optional().parse(options.body) ?? '{}'));
    expect(url.protocol).toBe('https:');
    expect(options.redirect).toBe('manual');
    expect([
      'r.inna.is',
      'inna.is',
      'heimdallur.inna.is',
      'innskra.island.is',
      'nam.inna.is',
    ]).toContain(url.hostname);
    calls.push({ url, method });

    if (headers.has('Authorization')) {
      expect(url.origin).toBe('https://inna.is');
      expect(headers.get('Authorization')).toBe(`Bearer ${token}`);
    }

    if (method === 'POST' && url.hostname === 'innskra.island.is')
      expect(headers.get('X-CSRF-TOKEN-IDS')).toBe('synthetic-csrf');

    switch (`${url.hostname}${url.pathname}`) {
      case 'r.inna.is/auth/island':
        return redirect(
          trap
            ? 'https://example.invalid/credential-trap'
            : 'https://heimdallur.inna.is/auth/island/login',
        );
      case 'heimdallur.inna.is/auth/island/login':
        return redirect('https://innskra.island.is/connect/authorize');
      case 'innskra.island.is/connect/authorize':
        return redirect(
          'https://innskra.island.is/app/login?ReturnUrl=' +
            encodeURIComponent('/connect/authorize/callback?state=synthetic'),
        );
      case 'innskra.island.is/app/login':
        return new Response('<html>synthetic phone shell</html>');
      case 'innskra.island.is/login/context':
        return Response.json(
          { identityProviderRestrictions: [] },
          { headers: { 'Set-Cookie': 'CSRF-TOKEN-IDS=synthetic-csrf; Path=/; Secure' } },
        );
      case 'innskra.island.is/login/phone':
        return Response.json({
          displayCode: '1234',
          verificationProperties: 'synthetic-verification',
        });
      case 'innskra.island.is/login/phone/check-device':
        expect(body).toEqual({
          returnUrl: encodeURIComponent('/connect/authorize/callback?state=synthetic'),
          userIdentifier: '5550000',
        });

        return Response.json({ isTwoFactorRequired: false, isNewLoginRestricted: false });
      case 'innskra.island.is/login/phone/authenticate':
        expect(body.verificationProperties).toBe('synthetic-verification');

        return Response.json({ userIdentifier: null, session: session('synthetic-first') });
      case 'innskra.island.is/login/phone/poll':
        expect(body.userIdentifier).toBeUndefined();
        expect(body.data).toBe(polls++ ? 'synthetic-next' : 'synthetic-first');

        return Response.json(
          polls === 1 ? session('synthetic-next', false, 3000) : session('synthetic-success', true),
        );
      case 'innskra.island.is/login/phone/signin':
        expect(body.session?.data).toBe('synthetic-success');
        expect(body.session?.isSuccess).toBe(true);

        return Response.json(
          { validReturnUrl: '/connect/authorize/callback?state=synthetic' },
          { headers: { 'Set-Cookie': 'innskra=synthetic-identity-cookie; Path=/; Secure' } },
        );
      case 'innskra.island.is/connect/authorize/callback':
        return redirect('https://heimdallur.inna.is/auth/island/callback');
      case 'heimdallur.inna.is/auth/island/callback':
        return redirect('https://innskra.island.is/connect/endsession');
      case 'innskra.island.is/connect/endsession':
        return redirect('https://innskra.island.is/logout?logoutId=synthetic');
      case 'innskra.island.is/logout':
        return new Response(
          '<a class="other PostLogoutRedirectUri" href="https://heimdallur.inna.is/auth/island/logout-callback?state=synthetic">Continue</a>',
        );
      case 'heimdallur.inna.is/auth/island/logout-callback':
        return redirect('https://inna.is/auth/island/callback?token=synthetic');
      case 'inna.is/auth/island/callback':
        return new Response(`<script>var jwt = "${token}"; store.set('id_token',jwt);</script>`);
      case 'inna.is/auth/access':
        return Response.json(access.map((entry) => Object.assign({ ssn: 'DO-NOT-SAVE' }, entry)));
      case 'inna.is/auth/user-terms-confirmed':
        return Response.json({ confirmed: terms });
      case 'inna.is/auth/system':
        expect(method).toBe('POST');
        expect(body).toEqual({});
        expect([...url.searchParams.keys()]).toEqual(['i', 'system', 'user_id', 'status']);
        selections.push(Object.fromEntries(url.searchParams));

        return Response.json({ url: 'http://nam.inna.is/auth/token?token=synthetic' });
      case 'nam.inna.is/auth/token':
        expect(headers.has('Authorization')).toBe(false);

        return redirect(
          'http://nam.inna.is/auth/system',
          'SESSION=synthetic-school; Path=/; Secure; HttpOnly',
        );
      case 'nam.inna.is/auth/system':
        return redirect(
          'http://nam.inna.is/Components/Students/Students.html',
          'XSRF-TOKEN=synthetic-xsrf; Path=/',
        );
      case 'nam.inna.is/Components/Students/Students.html':
        return new Response('<html>Synthetic student application</html>');
      default:
        throw new Error('Unexpected synthetic login request');
    }
  };

  return {
    calls,
    fetcher,
    setTerms: (value: boolean) => {
      terms = value;
    },
    selections,
    setAccess: (value: typeof access) => {
      access = value;
    },
    setTrap: () => {
      trap = true;
    },
  };
}
// ---- end of login.test.ts ----

// ---- rust-login.ts (41d6f98): the school session's user ----
/** The user Inna reports for the new school session: the saved default student, if any. */
const user = (userId: number) => ({
  userId,
  studentId: '2',
  schoolId: '3',
  studentName: 'Synthetic student',
  name: 'Synthetic guardian',
  schoolLong: 'Synthetic school',
  defaultTermId: '4',
  isGuardian: true,
  logInType: '2',
  olderThan18: false,
  registerAbsenceGuardian: '1',
  registerAbsenceUnder18: '0',
  registerAbsenceOver18: '1',
  registerAbsence: '1',
  student18RegisterAbsence: '1',
  registerLeave: '1',
  student18RegisterLeave: '1',
  registerIllnessTomorrow: '1',
});
// ---- end of rust-login.ts ----

// ---- the contract set ----

/** The fetch the contract upstream answers with; each case sets its own. */
let contractFetch = async (_url: string, _init: RequestInit): Promise<Response> => {
  throw new Error('Synthetic: no case upstream yet');
};

/** rust-inna.ts's and rust-login.ts's hosts. */
const CONTRACT_HOSTS = new Set([
  'nam.inna.is',
  'inna.is',
  'r.inna.is',
  'heimdallur.inna.is',
  'innskra.island.is',
]);

/**
 * rust-inna.ts's `upstream`: the case's fetch on loopback, given the Inna URL back with the
 * method, headers, body and `redirect: 'manual'`. A thrown error drops the connection, as a failed
 * fetch; one the case did not mean (a provider's failed expectation) fails the freeze.
 */
async function answerCase(incoming: IncomingMessage, out: ServerResponse): Promise<void> {
  const path = incoming.url ?? '/';
  const [, host = ''] = path.split('/');

  if (!CONTRACT_HOSTS.has(host)) {
    out.statusCode = 404;
    out.end();

    return;
  }

  const headers = new Headers();

  for (const [name, value] of Object.entries(incoming.headers))
    for (const item of [value ?? []].flat()) headers.append(name, item);

  const body = await bytesOf(incoming);
  const init: RequestInit = { method: incoming.method ?? 'GET', headers, redirect: 'manual' };

  if (body !== '') init.body = body;

  let response: Response;

  try {
    response = await contractFetch(`https://${path.slice(1)}`, init);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);

    if (!message.startsWith('Synthetic')) problems.push(`${path}: ${message}`);

    incoming.socket.destroy();

    return;
  }

  out.statusCode = response.status;

  for (const [name, value] of response.headers)
    if (name !== 'set-cookie') out.setHeader(name, value);

  const cookies = response.headers.getSetCookie();

  if (cookies.length) out.setHeader('set-cookie', cookies);

  out.end(Buffer.from(await response.arrayBuffer()));
}

const caseServer = createServer((incoming, out) => void answerCase(incoming, out));

await new Promise<void>((done) => caseServer.listen({ host: '127.0.0.1', port: 0 }, done));

const caseAddress = caseServer.address();

if (!(caseAddress instanceof Object)) throw new RangeError('No case upstream port.');

if (set === 'contract') target = `http://127.0.0.1:${caseAddress.port}`;

/** integration.test.ts's `fixture`: the cookie export each case imports. */
const EXPORT = JSON.stringify(
  ['SESSION', 'XSRF-TOKEN'].map((name) => ({
    name,
    value: name === 'SESSION' ? 'synthetic-session' : 'synthetic-xsrf',
    domain: 'nam.inna.is',
    path: '/',
    secure: true,
  })),
);

/** rust-login.ts's session saved before a sign-in that prefers its default student. */
const SAVED_EXPORT = JSON.stringify([
  { name: 'SESSION', value: 'synthetic-saved', domain: 'nam.inna.is', path: '/', secure: true },
  {
    name: 'XSRF-TOKEN',
    value: 'synthetic-saved-xsrf',
    domain: 'nam.inna.is',
    path: '/',
    secure: true,
  },
]);

const toolAnswer = z.looseObject({
  result: z
    .looseObject({
      isError: z.boolean().optional(),
      content: z.array(z.looseObject({ text: z.string() })).optional(),
      structuredContent: z.unknown().optional(),
    })
    .optional(),
  error: z.looseObject({ message: z.string() }).optional(),
});

type Called = { ok: true; value: unknown } | { ok: false; text: string };

const listedTools = z.object({
  result: z.object({
    tools: z.array(
      z.looseObject({
        name: z.string(),
        inputSchema: z.unknown(),
        annotations: z.looseObject({ readOnlyHint: z.boolean().optional() }).optional(),
      }),
    ),
  }),
});

/** The store's record options in this process, inside `inHome`. */
function recordOptions() {
  return {
    path: defaultSecretRecordPath('inna-mcp'),
    server: 'inna-mcp',
    profile: 'default',
    purpose: 'session',
    schema: 1,
    maxBytes: RECORD_MAX_BYTES,
    keys: defaultKeyProvider({ server: 'inna-mcp', profile: 'default' }),
  };
}

/** The store marker's generation, when there is a marker. */
function generation(home: string): number | undefined {
  const marker = join(home, '.config/inna-mcp/session.enc.marker');

  if (!existsSync(marker)) return undefined;

  return Number(/"generation":(\d+)/.exec(readFileSync(marker, 'utf8'))?.[1] ?? Number.NaN);
}

/**
 * A contract case's steps, recorded as it runs them. As the drop-in `InnaClient`, the tool calls
 * share one `serve` until the case closes it, and a CLI run may come between two of them.
 */
class Recorder {
  readonly steps: object[] = [];

  private server: Server | undefined;

  constructor(
    readonly home: string,
    readonly names: Names,
    private readonly env: Record<string, string>,
    private readonly serveArgs: string[],
  ) {}

  private journal() {
    return { journal: journals(this.home, this.names) };
  }

  file(file: string, text: string, permissions?: number): void {
    const step = permissions === undefined ? { file, text } : { file, text, permissions };
    writeStep(this.home, step);
    this.steps.push(step);
  }

  remove(path: string): void {
    rmSync(join(this.home, path));
    this.steps.push({ remove: path });
  }

  async cli(args: string[], stdin?: string) {
    const step = stdin === undefined ? { cli: args } : { cli: args, stdin };
    const ran = await runCli(this.home, this.names, this.env, step);
    this.steps.push({ ...ran, ...this.journal() });

    return ran;
  }

  /** integration.test.ts's `fixture`: a cookie export, imported. */
  async import(text = EXPORT): Promise<void> {
    this.file('cookies.json', text);
    const ran = await this.cli(['auth', 'import', '<home>/cookies.json']);
    assert.equal(ran.code, 0, ran.stderr);
  }

  /** Starts the case's `serve`; with `surface`, also its `initialize` and `tools/list` answers. */
  async open(surface = false): Promise<Server> {
    assert.equal(this.server, undefined, 'serve is already open');
    const server = new Server(this.home, this.env, this.serveArgs);
    const initialize = await server.initialize();
    const list = surface ? (await server.request('tools/list', {})).answer : undefined;

    this.steps.push(
      Object.assign(
        { open: true },
        this.serveArgs.length ? { args: this.serveArgs } : undefined,
        list === undefined ? undefined : { initialize, list },
      ),
    );

    this.server = server;

    return server;
  }

  /** The `tools/list` answer of a surface `open`. */
  listed(): z.infer<typeof listedTools>['result']['tools'] {
    const step = z.looseObject({ list: z.string() }).parse(this.steps.at(-1));

    return listedTools.parse(JSON.parse(step.list)).result.tools;
  }

  async call(name: string, args: Arguments = {}): Promise<Called> {
    const server = this.server ?? (await this.open());
    const start = exchanges.length;

    const { notifications, answer } = await server.request('tools/call', {
      name,
      arguments: placed(JSON.stringify(args), this.names),
    });

    leaks(this.home, answer + notifications.join(''));
    expectClean(answer);
    this.names.learn(answer);

    const step = Object.assign(
      { call: [name, args] satisfies [string, unknown] },
      notifications.length
        ? {
            notifications: notifications.map((line) =>
              z.unknown().parse(JSON.parse(this.names.fresh(line))),
            ),
          }
        : undefined,
      { answer: this.names.fresh(answer), exchanges: since(start, this.names) },
    );

    this.steps.push({ ...step, ...this.journal() });
    const message = toolAnswer.parse(JSON.parse(answer));

    if (message.result === undefined) return { ok: false, text: message.error?.message ?? '' };

    if (message.result.isError === true)
      return { ok: false, text: message.result.content?.[0]?.text ?? '' };

    return { ok: true, value: message.result.structuredContent };
  }

  /** Closes the case's `serve`, if open: its exit code and stderr once its input closed. */
  async close(): Promise<void> {
    const server = this.server;

    if (server === undefined) return;

    this.server = undefined;
    const { code, stderr } = await server.close();
    leaks(this.home, stderr);
    this.steps.push({ close: true, code, stderr: this.names.fresh(stderr), ...this.journal() });
  }

  /** The store as the replay must find it now, with the marker's generation; the record text. */
  async stored(): Promise<string | null> {
    const found = await stored(this.home, this.names);
    const marker = generation(this.home);
    this.steps.push(
      marker === undefined ? { stored: found } : { stored: found, generation: marker },
    );

    return 'record' in found ? found.record : null;
  }

  /** scratch.ts's `updateStored`, by the TypeScript store; the replay plants the home it left. */
  async updateStored(update: (text: string) => string): Promise<void> {
    await inHome(this.home, () =>
      withSecretRecord(recordOptions(), async (text) => update(text ?? 'null')),
    );
    this.steps.push({ plant: snapshot(this.home), operations: [...this.names.operations] });
  }
}

type Contract = {
  name: string;
  /** `serve`'s arguments. */
  args?: string[];
  env?: Record<string, string>;
  run: (c: Recorder) => Promise<void>;
};

const WRITE_ARGS = ['--allow-absence-writes'];

/** The drop-in harness's legacy session file, beside the store. */
const SESSION_FILE = { INNA_SESSION_FILE: '<home>/session.json' };

/** The contract cases' two days; the parity tables' `range` is a week. */
const days = { dateFrom: '2040-01-02', dateTo: '2040-01-03' };

/** integration.test.ts's `fixture`: the case's Inna, and the cookie export imported. */
async function begin(c: Recorder, inna = new Provider()): Promise<Provider> {
  contractFetch = inna.fetch;
  await c.import();

  return inna;
}

/** rust-login.ts's upstream: the case's sign-in, and Inna's user for the new school session. */
function signIn(p: ReturnType<typeof provider>, userId = 2): void {
  contractFetch = async (url, init) => {
    const { hostname, pathname } = new URL(url);

    if (hostname === 'nam.inna.is' && pathname === '/api/UserData/GetLoggedInUser')
      return Response.json(user(userId));

    return p.fetcher(url, init);
  };
}

function failed(called: Called, pattern: RegExp, at: string): void {
  expect(called.ok, at).toBe(false);

  if (!called.ok) expect(called.text, at).toMatch(pattern);
}

function succeeded<T>(called: Called, schema: z.ZodType<T>, at: string): T {
  if (!called.ok) throw new RangeError(`${at}: ${called.text}`);

  return schema.parse(called.value);
}

const anyOutput = z.unknown();

const READ_CALLS: [string, Arguments][] = [
  ['inna_session_status', {}],
  ['inna_list_students', {}],
  ['inna_get_overview', {}],
  ['inna_get_timetable', days],
  ['inna_get_assignments', {}],
  ['inna_get_assignment', { assignmentId: '5' }],
  ['inna_get_grades', {}],
  ['inna_get_course_grades', { groupId: '7' }],
  ['inna_get_attendance', {}],
  ['inna_get_materials', { groupId: '7' }],
  ['inna_get_messages', {}],
  ['inna_get_message', { messageId: '11', type: 'A' }],
  ['inna_get_absences', days],
  ['inna_absence_status', {}],
];

/** integration.test.ts:1120's `parseSaved`. */
const savedRecord = z.object({
  version: z.number(),
  jar: z.string(),
  account: z.object({ userId: z.number(), studentId: z.string(), schoolId: z.string() }),
  students: z.record(z.string(), z.object({ studentId: z.string(), studentName: z.string() })),
  pauseUntil: z.number(),
});

/** integration.test.ts:2365's `digits`. */
const digits = (index: number) => String(index).padStart(32, '9');

const TOO_MANY =
  'This Inna session holds more saved students than this version keeps. Run inna-mcp auth logout, then sign in again.';

const contracts: Contract[] = [
  {
    // integration.test.ts:460
    name: 'read-tools-round-trip',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);
      await c.open(true);
      const tools = c.listed();
      expect(tools.some((tool) => tool.name === 'inna_submit_absence')).toBe(false);

      expect(tools.map((tool) => tool.name).toSorted()).toEqual(
        READ_CALLS.map(([name]) => name).toSorted(),
      );

      expect(
        tools
          .filter((tool) => tool.annotations?.readOnlyHint === true)
          .map((tool) => tool.name)
          .toSorted(),
      ).toEqual(['inna_absence_status', 'inna_list_students']);

      const metadata = z.looseObject({ retrievedAt: z.string(), timeZone: z.string() });

      for (const [name, args] of READ_CALLS) {
        const shown = succeeded(await c.call(name, args), metadata, name);
        expect(shown.retrievedAt, name).toBe(new Date(NOW).toISOString());
        expect(shown.timeZone, name).toBe('UTC');
      }

      expect(p.calls.every((call) => call.method === 'GET')).toBe(true);

      expect(
        p.calls.some((call) =>
          /Mark|SetUserOptions|Download|SubmitAssignment/.test(call.url.pathname),
        ),
      ).toBe(false);

      const timetable = p.calls.find((call) => call.url.pathname === '/api/Timetable/GetTimetable');
      expect(timetable?.url.searchParams.get('date_from')).toBe('02.01.2040');
      expect(timetable?.url.searchParams.get('student_id')).toBe('2');
    },
  },
  {
    // integration.test.ts:590
    name: 'message-continuation',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);

      const messages = Array.from({ length: 41 }, (_, i) => ({
        messagesId: String(i + 1),
        table: 'A',
        sender: 'Synthetic sender',
        date: '02.01.2040 9:05',
        status: 'S',
      }));

      let broken: 'empty' | 'duplicate' | 'count' | undefined;

      contractFetch = async (url, options) => {
        const response = await p.fetch(url, options);
        const endpoint = new URL(url);

        if (endpoint.pathname !== '/api/Messages/GetReceivedMessages') return response;

        const start = Number(endpoint.searchParams.get('rowFrom'));
        const end = Number(endpoint.searchParams.get('rowTo'));

        const rows =
          broken === 'empty'
            ? []
            : broken === 'duplicate'
              ? [messages[0], messages[0]]
              : messages.slice(start - 1, end - 1);

        return Response.json({ count: broken === 'count' ? 0 : messages.length, messages: rows });
      };

      const page = z.looseObject({
        messages: z.array(z.looseObject({ messagesId: z.string() })),
        nextRowFrom: z.number().nullable(),
      });

      const seen = new Set<string>();

      for (const [rowFrom, rowTo, length, next] of [
        [1, 21, 20, 21],
        [21, 41, 20, 41],
        [41, 61, 1, null],
      ] as const) {
        const shown = succeeded(
          await c.call('inna_get_messages', { rowFrom, rowTo }),
          page,
          `rows ${rowFrom}`,
        );

        expect(shown.messages).toHaveLength(length);
        expect(shown.nextRowFrom).toBe(next);

        for (const message of shown.messages) seen.add(message.messagesId);
      }

      expect(seen.size).toBe(41);

      for (const failure of ['empty', 'duplicate', 'count'] as const) {
        broken = failure;
        failed(
          await c.call('inna_get_messages', { rowFrom: 1, rowTo: 21 }),
          /incomplete|inconsistent/,
          failure,
        );
      }

      expect(p.posts).toBe(0);
    },
  },
  {
    // integration.test.ts:781: each history in its own client, as the case's two fixtures.
    name: 'illness-history-blocks-requests',
    args: WRITE_ARGS,
    env: SESSION_FILE,
    run: async (c) => {
      for (const date of ['02.01.2040 malformed time', '02.01.2040']) {
        const p = await begin(c);

        contractFetch = async (url, options) => {
          const response = await p.fetch(url, options);

          return new URL(url).pathname === '/api/RegisterAbsence/GetStudentRegisteredAbsences'
            ? Response.json([{ id: 1, date, statusCode: 0, allDay: '1', classes: [] }])
            : response;
        };

        for (const kind of ['sick', 'leave'] as const)
          failed(
            await c.call('inna_prepare_absence', { ...request, kind }),
            /unrecognized absence date|already registered/,
            `${date} ${kind}`,
          );

        expect(p.posts).toBe(0);
        await c.close();
      }
    },
  },
  {
    // integration.test.ts:806
    name: 'dated-feeds',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);

      const responses = new Map([
        [
          '/api/ModulesAndBooklist/GetModulesAndBooklist',
          JSON.stringify([
            {
              moduleId: '1',
              moduleTermId: '2',
              moduleName: 'Synthetic',
              moduleName2: 'Synthetic',
              subjectName: 'Synthetic',
              groupId: '7',
              groupName: '1',
              termId: '4',
              dateFrom: '02.01.2040',
              dateTo: 'bad',
            },
          ]),
        ],
        [
          '/api/Homework/GetStudentHomework',
          JSON.stringify([
            { id: 1, date: '02.01.2040 9:05', moduleName: 'Synthetic', text: '<p>Synthetic</p>' },
          ]),
        ],
        [
          '/api/StudentGrades/GetStudentGrades',
          JSON.stringify([
            {
              moduleTermId: '2',
              termId: '4',
              moduleName: 'Synthetic',
              subjectName: 'Synthetic',
              units: '5',
              status: '1',
              show: true,
              termCode: 'Synthetic',
              dateFinished: '',
            },
          ]),
        ],
        [
          '/api/RegisterAbsence/GetStudentRegisteredAbsences',
          JSON.stringify([
            {
              id: 1,
              date: '02.01.2040',
              statusCode: 0,
              allDay: '1',
              classes: [
                {
                  id: 1,
                  date: '02.01.2040',
                  timeFrom: '9:05',
                  timeTo: '10:00',
                  class: 'Synthetic',
                },
              ],
            },
          ]),
        ],
        [
          '/api/RegisterAbsence/GetLeaves',
          JSON.stringify([
            {
              id: 2,
              dateFrom: '02.01.2040',
              dateTo: '03.01.2040',
              created: '01.01.2040 23:59:59',
              leaveType: '3',
              status: '0',
              statusCode: 0,
              reasonForLeave: 'Synthetic',
              createdBy: 'Synthetic',
              classes: [],
            },
          ]),
        ],
      ]);

      contractFetch = async (url, options) => {
        const response = await p.fetch(url, options);
        const body = responses.get(new URL(url).pathname);

        return body === undefined
          ? response
          : new Response(body, { headers: { 'Content-Type': 'application/json' } });
      };

      const overview = succeeded(
        await c.call('inna_get_overview'),
        z.looseObject({
          courses: z.array(z.looseObject({ dateTo: z.string(), dates: z.unknown() })),
        }),
        'overview',
      );

      expect(overview.courses[0]?.dateTo).toBe('bad');

      expect(overview.courses[0]?.dates).toEqual({
        dateFrom: { iso: '2040-01-02', status: 'parsed' },
        dateTo: { iso: null, status: 'unrecognized' },
      });

      for (const [name, args] of [
        ['inna_get_timetable', days],
        ['inna_get_assignments', { type: 'assignments' }],
        ['inna_get_assignment', { assignmentId: '5' }],
        ['inna_get_grades', {}],
        ['inna_get_course_grades', { groupId: '7' }],
        ['inna_get_attendance', {}],
        ['inna_get_materials', { groupId: '7' }],
        ['inna_get_message', { messageId: '11', type: 'A' }],
        ['inna_get_absences', days],
      ] satisfies [string, Arguments][])
        succeeded(await c.call(name, args), anyOutput, name);

      expect(p.posts).toBe(0);
    },
  },
  {
    // integration.test.ts:1177: the TypeScript client read the default user id from the store.
    name: 'default-user-id',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);

      const userId = (text: string | null) =>
        savedRecord.parse(JSON.parse(text ?? 'null')).account.userId;

      expect(userId(await c.stored())).toBe(1);

      succeeded(await c.call('inna_get_overview', { studentKey: SIBLING }), anyOutput, 'overview');
      expect(userId(await c.stored())).toBe(1);
      const calls = p.calls.length;
      expect((await c.cli(['auth', 'logout'])).code).toBe(0);
      expect(await c.stored()).toBe('null');
      expect(p.calls).toHaveLength(calls);
    },
  },
  {
    // integration.test.ts:1242
    name: 'students-listed-without-switching',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);

      const listing = z.looseObject({
        students: z.array(z.looseObject({})),
        context: z.looseObject({ studentId: z.string() }),
        retrievedAt: z.string(),
      });

      const listed = succeeded(await c.call('inna_list_students'), listing, 'list');

      expect(listed.students).toEqual([
        {
          studentKey: '1',
          title: 'Synthetic role',
          schoolName: 'Synthetic school',
          schoolId: '3',
          selected: true,
          isDefault: true,
          studentId: '2',
          studentName: 'Synthetic student',
        },
        {
          studentKey: SIBLING,
          title: 'Synthetic role',
          schoolName: 'Synthetic second school',
          schoolId: '8',
          selected: false,
          isDefault: false,
          studentName: 'Synthetic sibling',
        },
      ]);

      expect(listed.context.studentId).toBe('2');
      expect(listed.retrievedAt).toBe(new Date(NOW).toISOString());
      expectClean((await c.stored()) ?? '');
      expect(p.paths()).toEqual(['/api/UserData/GetLoggedInUser', '/api/UserData/GetLoggedInUser']);
      succeeded(
        await c.call('inna_get_timetable', { ...days, studentKey: SIBLING }),
        anyOutput,
        'timetable',
      );
      const after = succeeded(await c.call('inna_list_students'), listing, 'list after');
      expect(after.context.studentId).toBe('6');
      expect(p.switches).toBe(1);
    },
  },
  {
    // integration.test.ts:1289
    name: 'single-default-context',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);
      p.order = ['9'];

      const listed = succeeded(
        await c.call('inna_list_students'),
        z.looseObject({ students: z.array(z.looseObject({})) }),
        'list',
      );

      expect(listed.students).toEqual([
        {
          schoolName: 'Synthetic school',
          schoolId: '3',
          selected: true,
          isDefault: true,
          studentId: '2',
          studentName: 'Synthetic student',
        },
      ]);

      succeeded(await c.call('inna_get_overview'), anyOutput, 'overview');
      failed(
        await c.call('inna_get_overview', { studentKey: SIBLING }),
        /not in this Inna session/,
        'sibling',
      );
      p.order = ['1', '1'];
      failed(await c.call('inna_list_students'), /no usable student list/, 'duplicate list');
      failed(
        await c.call('inna_get_overview', { studentKey: '1' }),
        /not in this Inna session/,
        'key 1',
      );
      succeeded(await c.call('inna_get_overview'), anyOutput, 'overview again');
      expect(p.switches).toBe(0);
    },
  },
  {
    // integration.test.ts:1417
    name: 'unknown-student-keys',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);

      for (const key of ['999', '9'])
        failed(
          await c.call('inna_get_overview', { studentKey: key }),
          /not in this Inna session/,
          key,
        );

      failed(await c.call('inna_get_overview', { studentKey: 'abc' }), /./, 'abc');
      expect(p.switches).toBe(0);
      expect(p.paths()).not.toContain('/api/StudentTerms/GetStudentTerms');
    },
  },
  {
    // integration.test.ts:1642
    name: 'sibling-tool-round-trips',
    args: WRITE_ARGS,
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);
      await c.open(true);
      const keyed = { studentKey: SIBLING };

      const calls: [string, Arguments][] = [
        ['inna_list_students', {}],
        ['inna_session_status', keyed],
        ['inna_get_overview', keyed],
        ['inna_get_timetable', { ...days, ...keyed }],
        ['inna_get_assignments', keyed],
        ['inna_get_assignment', { assignmentId: '5', ...keyed }],
        ['inna_get_grades', keyed],
        ['inna_get_course_grades', { groupId: '7', ...keyed }],
        ['inna_get_attendance', keyed],
        ['inna_get_materials', { groupId: '7', ...keyed }],
        ['inna_get_messages', keyed],
        ['inna_get_message', { messageId: '11', type: 'A', ...keyed }],
        ['inna_get_absences', { ...days, ...keyed }],
        ['inna_prepare_absence', { ...request, ...keyed }],
        ['inna_absence_status', {}],
      ];

      const tools = c.listed();

      expect(tools.map((tool) => tool.name).toSorted()).toEqual(
        [...calls.map(([name]) => name), 'inna_submit_absence'].toSorted(),
      );

      for (const tool of tools)
        expect(JSON.stringify(tool.inputSchema).includes('studentKey'), tool.name).toBe(
          !['inna_list_students', 'inna_absence_status', 'inna_submit_absence'].includes(tool.name),
        );

      const keyedOutput = z.looseObject({
        retrievedAt: z.string(),
        timeZone: z.literal('UTC'),
        context: z.object({ studentId: z.string() }).loose().optional(),
        account: z.object({ studentId: z.string() }).loose().optional(),
      });

      for (const [name, args] of calls) {
        const result = succeeded(await c.call(name, args), keyedOutput, name);

        if (name !== 'inna_list_students')
          expect((result.context ?? result.account)?.studentId, name).toBe('6');
      }

      expect(p.switches).toBe(1);
      const approval = { operationId: '<operation>', confirm: true };
      failed(await c.call('inna_submit_absence', { ...approval, ...keyed }), /./, 'keyed submit');
      succeeded(await c.call('inna_get_overview'), anyOutput, 'overview');

      const submitted = succeeded(
        await c.call('inna_submit_absence', approval),
        z.looseObject({ account: z.looseObject({ studentId: z.string() }) }),
        'submit',
      );

      expect(submitted.account.studentId).toBe('6');
      expect(p.posts).toBe(1);
      expect(p.switches).toBe(3);
    },
  },
  {
    // integration.test.ts:1916
    name: 'keep-alive-opt-out-flag',
    env: SESSION_FILE,
    run: async (c) => {
      for (const args of [
        ['auth', 'status', '--no-keep-alive'],
        ['auth', 'logout', '--no-keep-alive'],
      ]) {
        const ran = await c.cli(args);
        expect(ran.code).toBe(1);
        expect(ran.stdout).toBe('');
        expect(ran.stderr).toBe('Invalid command. Run inna-mcp --help.\n');
      }
    },
  },
  {
    // integration.test.ts:2396: the unmigrated half's own store is this home's, emptied.
    name: 'too-many-saved-students',
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);
      let original = '';

      const withStudents = (count: number) =>
        JSON.stringify({
          ...savedRecord.parse(JSON.parse(original)),
          students: Object.fromEntries(
            Array.from({ length: count }, (_, index) => [
              digits(index),
              {
                userId: index + 100,
                studentId: digits(index),
                schoolId: '3',
                studentName: 'Synthetic',
              },
            ]),
          ),
        });

      await c.updateStored((text) => {
        original = text;

        return withStudents(64);
      });

      await c.stored();
      const before = generation(c.home) ?? Number.NaN;
      // Learning a 65th student is refused; only the jar is written back.
      expect(await c.call('inna_get_overview', { studentKey: SIBLING })).toEqual({
        ok: false,
        text: TOO_MANY,
      });
      expect(generation(c.home)).toBe(before + 1);
      const after = savedRecord.parse(JSON.parse((await c.stored()) ?? 'null'));
      expect(Object.keys(after.students)).toHaveLength(64);
      await c.updateStored(() => withStudents(65));
      const calls = p.calls.length;
      const refused = await c.call('inna_get_overview');
      expect(refused).toEqual({ ok: false, text: TOO_MANY });
      await c.stored();
      await c.close();
      c.remove('.config/inna-mcp/session.enc');
      c.remove('.config/inna-mcp/session.enc.marker');
      c.remove('.local/share/family-mcp/keys/inna-mcp.default.key');
      // freeze: the jar's wall-clock times at the scenario clock, so the file is the same each run.
      c.file(
        'session.json',
        withStudents(65).replaceAll(
          /20[23]\d-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z/g,
          new Date(NOW).toISOString(),
        ),
      );
      expect(await c.call('inna_get_overview')).toEqual({ ok: false, text: TOO_MANY });
      await c.close();
      const migrate = await c.cli(['auth', 'migrate']);
      expect({ code: migrate.code, stderr: migrate.stderr }).toEqual({
        code: 1,
        stderr: `${TOO_MANY}\n`,
      });
      expect(p.calls).toHaveLength(calls);
    },
  },
  {
    // login.test.ts:214: each refusal its own sign-in, as the case's three providers.
    name: 'phone-login-refusals',
    env: { INNA_SESSION_FILE: '<home>/legacy/session.json' },
    run: async (c) => {
      const p = provider();
      p.setTrap();
      signIn(p);
      const trapped = await c.cli(['auth', 'login'], '5550000\n');
      expect(trapped.code).toBe(1);
      expect(trapped.stderr).toMatch(/unexpected destination/);
      expect(p.calls).toHaveLength(1);

      for (const reason of ['terms', 'schools']) {
        const q = provider();

        if (reason === 'terms') q.setTerms(false);
        else
          q.setAccess([
            { system: 2, user_id: 7, status: 1, is_access: true },
            { system: 1, user_id: 8, status: 1, is_access: false },
          ]);

        signIn(q);
        const refused = await c.cli(['auth', 'login'], '5550000\n');
        expect(refused.code).toBe(1);
        expect(refused.stderr).toMatch(
          reason === 'terms' ? /accept Inna terms yourself/ : /Select the intended school/,
        );

        expect(
          q.calls.some((call) => call.method === 'POST' && call.url.hostname === 'inna.is'),
        ).toBe(false);
      }
    },
  },
  {
    // login.test.ts:242: a preferred user id is the default student of a session saved first.
    name: 'phone-login-student-context',
    env: { INNA_SESSION_FILE: '<home>/legacy/session.json' },
    run: async (c) => {
      const access = [
        { system: 2, user_id: 7, status: 1, is_access: true },
        { system: 1, user_id: 8, status: 1, is_access: false },
        { system: 1, user_id: 2, status: 3, is_access: true },
        { system: 1, user_id: 5, status: 4, is_access: true },
      ];

      const first = { i: '2', system: '1', user_id: '2', status: '3' };

      for (const [index, [preferredUserId, selection]] of (
        [
          [undefined, first],
          [5, { i: '3', system: '1', user_id: '5', status: '4' }],
          [2, first],
          [99, first],
          [8, first],
          [7, first],
        ] as const
      ).entries()) {
        const p = provider();
        p.setAccess(access);
        signIn(p, preferredUserId ?? 2);

        if (index > 0) expect((await c.cli(['auth', 'logout'])).code).toBe(0);

        if (preferredUserId !== undefined) await c.import(SAVED_EXPORT);

        const ran = await c.cli(['auth', 'login'], '5550000\n');
        expect(ran.code, ran.stderr).toBe(0);
        expect({ preferredUserId, selections: p.selections }).toEqual({
          preferredUserId,
          selections: [selection],
        });
      }
    },
  },
  {
    // browser-login.test.ts:729: a browser file that is never started.
    name: 'browser-options-only-with-google-login',
    env: { ...SESSION_FILE, INNA_BROWSER: '<home>/no-browser-selected', DISPLAY: ':0' },
    run: async (c) => {
      c.file('browser', '#!/bin/sh\nexit 1\n', 0o755);
      const browser = '<home>/browser';

      for (const args of [
        ['--google'],
        ['serve', '--google'],
        ['auth', '--google'],
        ['auth', 'status', '--google'],
        ['auth', 'logout', '--google'],
        ['auth', 'import', '<home>/cookies.json', '--google'],
        ['auth', 'login', 'extra', '--google'],
        ['auth', 'login', '--timeout', '5'],
        ['auth', 'login', '--browser', browser],
        ['auth', 'login', '--timeout', '5', '--browser', browser],
        ['auth', 'status', '--timeout', '5'],
        ['serve', '--browser', browser],
        ['--timeout', '5'],
        ['auth', 'login', '--google', '--browser', browser, '--no-keep-alive'],
        ['auth', 'login', '--google', '--browser', browser, '--allow-absence-writes'],
      ]) {
        const ran = await c.cli(args);
        expect({ args, stderr: ran.stderr }).toEqual({
          args,
          stderr: 'Invalid command. Run inna-mcp --help.\n',
        });
        expect(ran.stdout).toBe('');
        expect(ran.code).toBe(1);
      }

      for (const timeout of ['0', '-1', '1.5', 'soon', '']) {
        const ran = await c.cli([
          'auth',
          'login',
          '--google',
          '--browser',
          browser,
          `--timeout=${timeout}`,
        ]);

        expect({ timeout, stderr: ran.stderr }).toEqual({
          timeout,
          stderr: 'Provide a positive whole number for --timeout.\n',
        });
        expect(ran.stdout).toBe('');
        expect(ran.code).toBe(1);
      }
    },
  },
  {
    // The two tools no other frozen case gives an invalid input, and an unknown key for each of
    // the 16: with parity.ts's, every tool's input validation is frozen (ledger :1642).
    name: 'strict-inputs-of-every-other-tool',
    args: WRITE_ARGS,
    env: SESSION_FILE,
    run: async (c) => {
      const p = await begin(c);
      await c.open(true);

      const inputs: [string, Arguments][] = [
        ['inna_get_attendance', { termId: 5 }],
        ['inna_get_attendance', { termId: 'x' }],
        ['inna_get_materials', {}],
        ['inna_get_materials', { groupId: 'x' }],
        ...c.listed().map((tool): [string, Arguments] => [tool.name, { q: 1 }]),
      ];

      expect(inputs).toHaveLength(20);

      for (const [name, args] of inputs)
        failed(
          await c.call(name, args),
          /Input validation error/,
          `${name} ${JSON.stringify(args)}`,
        );

      expect(p.calls).toHaveLength(1);
    },
  },
];

async function runContract(contract: Contract) {
  const home = join(scratch, contract.name);
  mkdirSync(home, { recursive: true, mode: 0o700 });
  const names = new Names(home, false);
  const start = exchanges.length;
  writeFileSync(clock, String(NOW));
  const place = (text: string) => text.replaceAll('<home>', home);

  const env = Object.fromEntries(
    Object.entries(contract.env ?? {}).map(([name, value]) => [name, place(value)]),
  );

  const recorder = new Recorder(home, names, env, contract.args ?? []);

  try {
    await contract.run(recorder);
  } finally {
    await recorder.close();
  }

  assert.deepEqual(problems.splice(0), [], `${contract.name}: the upstream saw a problem`);

  const frozen: Frozen = Object.assign(
    { steps: recorder.steps, id: `inna/contract/${contract.name}` },
    contract.env ? { env: contract.env } : undefined,
    { requests: exchanges.length - start, files: files(home, names) },
  );

  return { ...frozen, ...(await stored(home, names)) };
}

try {
  mkdirSync(output, { recursive: true });
  const cases = set === 'parity' ? scenarios : contracts;

  for (const item of cases) {
    if (only !== undefined && item.name !== only) continue;

    const frozen = 'run' in item ? await runContract(item) : await run(item);
    writeFileSync(join(output, `${item.name}.json`), pack(layout(frozen)));
    process.stdout.write(`${item.name}: ${frozen.requests} requests\n`);
  }
} finally {
  await fake.close();
  proxyServer.close();
  caseServer.close();
  rmSync(scratch, { recursive: true, force: true });
}
