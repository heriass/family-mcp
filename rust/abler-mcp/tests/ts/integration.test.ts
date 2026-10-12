// packages/abler-mcp/test/integration.test.ts against the Rust binary, run by tests/browser.rs
// from packages/abler-mcp. Changed only where a case injects into the TypeScript process: the
// drop-ins of ./rust-abler.ts run the binary instead, and each other change is marked `Rust:`.
// The plaintext `saveSession`, `importCookies`, `loadSession`, `withSession` and `sessionStorage`
// stay TypeScript: they set up and inspect the files the two languages share.
import { test, expect } from 'bun:test';
import assert from 'node:assert/strict';
import {
  chmod,
  link,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  symlink,
  utimes,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, resolve, join } from 'node:path';

import { SafeError } from '@family-mcp/mcp-runtime';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';
import * as z from 'zod/v4';

import {
  importCookies,
  loadSession,
  ORIGIN,
  saveSession,
  sessionStorage,
  withSession,
  type Slot,
} from '../../../../packages/abler-mcp/src/auth.js';
import { createCdpMock } from '../../../../packages/abler-mcp/test/cdp-mock.js';
import {
  filesContaining,
  useScratchStore,
} from '../../../../packages/abler-mcp/test/scratch.js';
import {
  AblerClient,
  captureAndSave,
  failing,
  KeyFile as FakeKeyProvider,
  logoutSession,
  migrateSession,
  NOWHERE,
  onPreloadOutput,
  preloadUpstream,
  retryCandidate,
  saveVerifiedSession,
  serveStdio,
  spawnCli,
  statusCli,
  upstream as serveUpstream,
  verifier,
} from './rust-abler.js';

const rustBinary = process.env.ABLER_RUST_BINARY!;

const store = useScratchStore();

const requestBodySchema = z.object({
  operationName: z.string(),
  variables: z.object({
    first: z.number().optional(),
    cursor: z.string().nullable().optional(),
    filter: z
      .object({ participant: z.array(z.string()).optional() })
      .passthrough()
      .optional(),
  }),
});

const eventBase = {
  name: 'Practice',
  from: '2026-09-11T16:00:00Z',
  to: '2026-09-11T17:00:00Z',
  ageGroup: { id: 'age-group', name: 'Team' },
  currentPlayerAttendance: [],
};

/** The jar the store (or, before migration, the plaintext file) holds now. */
const saved = (path: string, slot: Slot = 'current') =>
  withSession(path, slot, new AbortController().signal, async (jar) => jar);

const cookie = {
  name: 'refreshToken',
  value: 'private-refresh',
  domain: 'www.abler.io',
  path: '/',
  expires: Date.now() / 1000 + 3600,
};

test('auth import rejects unsafe files and oversized file/stdin inputs before verification', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-import-input-'));
  const source = join(directory, 'cookies.json');
  const path = join(directory, 'session.json');
  const preload = join(directory, 'offline.ts');

  try {
    await writeFile(preload, `globalThis.fetch = () => { throw new Error('UNEXPECTED_FETCH'); };`);
    await writeFile(source, JSON.stringify([cookie]), { mode: 0o600 });

    const run = (argument: string, input = '') =>
      spawnCli(preload, ['auth', 'import', argument], {
        env: { ...process.env, ABLER_SESSION_FILE: path },
        stdin: new Blob([input]),
        stdout: 'pipe',
        stderr: 'pipe',
      });

    const rejected = async (argument: string, diagnostic: string, input = '') => {
      const child = await run(argument, input);

      const [code, out, error] = await Promise.all([
        child.exited,
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
      ]);

      expect(code).toBe(1);
      expect(out).toBe('');
      expect(error).toContain(diagnostic);
      expect((await readdir(directory)).some((name) => name.startsWith('session.json'))).toBe(
        false,
      );
    };

    if (process.platform !== 'win32') {
      // chmod after creation so the test remains adversarial under umask 077.
      for (const mode of [0o644, 0o666]) {
        await chmod(source, mode);
        await rejected(source, 'owner-only');
        expect((await stat(source)).mode & 0o777).toBe(mode);
      }

      await chmod(source, 0o600);
      const alias = join(directory, 'symlink.json');
      await symlink(source, alias);
      await rejected(alias, 'symlink');
    }

    const hard = join(directory, 'hardlink.json');
    await link(source, hard);
    await rejected(hard, 'hard link');
    await rejected(source, 'hard link');
    await rm(hard);
    await rejected(directory, 'regular file');
    await rejected(join(directory, 'missing.json'), 'Cannot read');
    const oversized = ' '.repeat(4 * 1024 * 1024 + 1);
    await writeFile(source, oversized);
    await rejected(source, '4 MiB limit');
    await rejected('-', '4 MiB limit', oversized);
    // Byte limits also apply to non-ASCII JSON, independently of character count.
    await rejected('-', '4 MiB limit', 'é'.repeat(2 * 1024 * 1024 + 1));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('auth import accepts private browser exports and stdin at the byte limit', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-valid-import-'));
  const source = join(directory, 'cookies.json');
  const path = join(directory, 'session.json');
  const preload = join(directory, 'offline.ts');

  try {
    await writeFile(
      preload,
      `globalThis.fetch = async (url, init) => {
      if (url === 'https://www.abler.io/oauth/token') {
        if (!new Headers(init.headers).get('cookie')?.includes('refreshToken=private-refresh'))
          throw new Error('Missing imported credential');
        return Response.json({ access_token: 'offline-access' }, {
          headers: { 'Set-Cookie': 'id_token=offline-access; Path=/; Max-Age=600' },
        });
      }
      if (url === 'https://www.abler.io/graphql' && JSON.parse(init.body).operationName === 'SessionStatus')
        return Response.json({ data: { me: { id: 'offline-parent', displayName: 'Parent' } } });
      throw new Error('UNEXPECTED_FETCH');
    };`,
    );
    const { expires, ...browserCookie } = cookie;

    const exported = JSON.stringify({
      cookies: [
        { ...browserCookie, expirationDate: expires },
        { name: '_analytics', value: 'é' },
      ],
      origins: [],
    });

    const atLimit = exported + ' '.repeat(4 * 1024 * 1024 - Buffer.byteLength(exported));
    await writeFile(source, atLimit, { mode: 0o600 });

    for (const argument of [source, '-']) {
      const child = await spawnCli(preload, ['auth', 'import', argument], {
        env: { ...process.env, ABLER_SESSION_FILE: path },
        stdin: new Blob([atLimit]),
        stdout: 'pipe',
        stderr: 'pipe',
      });

      const [code, out, error] = await Promise.all([
        child.exited,
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
      ]);

      expect(code).toBe(0);
      expect(error).toBe('');
      expect(out).toContain('saved and verified');
      expect(await (await saved(path)).getCookieString(ORIGIN)).toContain(
        'id_token=offline-access',
      );
      expect(await Bun.file(path).exists()).toBe(false);
      expect(
        await filesContaining(['private-refresh', 'offline-access', '_analytics'], store.home),
      ).toEqual([]);
    }

    expect(await readFile(source, 'utf8')).toBe(atLimit);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('refresh and GraphQL reject malformed upstream cookie domains with a fixed SafeError', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-cookie-error-'));
  const path = join(directory, 'session.json');
  const calls: string[] = [];

  const client = new AblerClient(path, async (url) => {
    calls.push(url);

    return Response.json(
      {},
      {
        headers: { 'Set-Cookie': 'id_token=offline; Domain=private-domain-marker.example; Path=/' },
      },
    );
  });

  try {
    await saveSession(path, await importCookies([cookie, { ...cookie, name: 'id_token' }]));
    const original = await readFile(path, 'utf8');

    for (const forceRefresh of [false, true]) {
      await assert.rejects(client.status(forceRefresh), (error) => {
        assert(error instanceof SafeError);
        expect(error.message).toBe('Abler returned an invalid authentication cookie.');

        return true;
      });
      expect(await readFile(path, 'utf8')).toBe(original);
    }

    expect(calls).toEqual([`${ORIGIN}/graphql`, `${ORIGIN}/oauth/token`]);
  } finally {
    await client.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('CLI hides unreviewed exceptions and preserves safe usage, import, and browser diagnostics', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-cli-errors-'));
  const path = join(directory, 'session.json');
  const preload = join(directory, 'offline.ts');
  const marker = 'PRIVATE_EXCEPTION_MARKER';
  const offline = `globalThis.fetch = async () => { throw new Error('${marker}'); };`;

  const check = async (args: string[], expected: string, mock = offline, input = '') => {
    await writeFile(preload, mock);

    const child = await spawnCli(preload, args, {
      env: { ...process.env, ABLER_SESSION_FILE: path, TMPDIR: directory },
      stdin: new Blob([input]),
      stdout: 'pipe',
      stderr: 'pipe',
    });

    const [code, out, error] = await Promise.all([
      child.exited,
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
    ]);

    expect(code).toBe(1);
    expect(out).toBe('');
    expect(error.trim()).toBe(expected);
    expect(error).not.toContain(marker);
  };

  try {
    await saveSession(path, await importCookies([cookie, { ...cookie, name: 'id_token' }]));
    await check(
      ['auth', 'status'],
      'Abler returned an invalid authentication cookie.',
      `globalThis.fetch = async () => Response.json({}, {
        headers: { 'Set-Cookie': 'id_token=offline; Domain=${marker}.example; Path=/' },
      });`,
    );
    await check(
      ['auth', 'status'],
      'Abler MCP failed.',
      `globalThis.fetch = async () => Response.json({ data: { me: { id: '${marker}', displayName: null } } });`,
    );

    // Unknown errors (including a forged name) and non-Error throws must not become safe messages.
    // Rust: the three throws are one behaviour of the binary, a stdin it cannot read, so the forged
    // name tests nothing more; and a marker that only a throwing mock carries never leaves this
    // process, so `not.toContain(marker)` is a real check only for the two response mocks above.
    for (const thrown of [
      `new Error('${marker}')`,
      `Object.assign(new Error('${marker}'), { name: 'SafeError' })`,
      `'${marker}'`,
    ]) {
      await check(
        ['auth', 'import', '-'],
        'Abler MCP failed.',
        `${offline}
        process.stdin[Symbol.asyncIterator] = async function* () { throw ${thrown}; };`,
      );
    }

    await check(
      ['auth', 'import', '-'],
      'Cookie JSON input exceeds the 4 MiB limit.',
      `${offline}
        process.stdin[Symbol.asyncIterator] = async function* () {
          yield Buffer.alloc(4 * 1024 * 1024);
          yield Buffer.from('x');
          throw new Error('${marker} read past the input limit');
        };`,
    );
    await check(['auth', 'import'], 'Provide a cookie JSON file, or - for stdin.');
    await check(
      ['auth', 'import', '-'],
      'Import failed: provide valid browser cookie JSON containing an unexpired Abler refreshToken.',
      offline,
      marker,
    );
    await check(
      ['auth', 'login', '--timeout', '0'],
      'Provide a positive whole number for --timeout.',
    );
    await check(
      ['auth', 'status', '--timeout', '1'],
      'The browser, timeout, and keep-browser options are only valid with auth login.',
    );
    await check(['unknown'], 'Invalid command. Run abler-mcp --help for usage.');
    await check([`--${marker}`], 'Invalid command-line options. Run abler-mcp --help for usage.');
    await check(
      ['auth', 'capture', marker],
      'Use a loopback Chrome debugging URL, such as http://127.0.0.1:9222.',
    );
    await check(['auth', 'capture'], 'Cannot connect to Chrome debugging.');
    await check(
      ['auth', 'capture'],
      'Invalid Chrome debugging response.',
      `globalThis.fetch = async () => new Response('${marker}');`,
    );
    await check(
      ['auth', 'login', '--browser', join(directory, 'missing-browser')],
      "No Chromium-family browser found. Install Chrome, Chromium, Brave, or Edge, or set ABLER_BROWSER/--browser to its executable. Use 'abler-mcp auth capture <URL>' or 'abler-mcp auth import <file>'.",
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('private cookie import, renewal, pagination, validation, and safe failures', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-test-'));
  const path = join(directory, 'session.json');

  try {
    // Rust: `auth import -` in place of `importCookies`; the CLI's one reviewed message stands
    // for importCookies' own, so /Invalid/ is that message too.
    const importCookiesCli = async (input: object) => {
      const child = await spawnCli(undefined, ['auth', 'import', '-'], {
        env: { ...process.env, ABLER_SESSION_FILE: path },
        stdin: new Blob([JSON.stringify(input)]),
        stdout: 'pipe',
        stderr: 'pipe',
      });

      expect(await child.exited).toBe(1);

      throw new Error((await new Response(child.stderr).text()).trim());
    };

    const rejectedImport =
      'Import failed: provide valid browser cookie JSON containing an unexpired Abler refreshToken.';

    await assert.rejects(
      importCookiesCli({ cookies: [{ ...cookie, domain: 'unrelated.example' }] }),
      /refreshToken/,
    );
    await assert.rejects(importCookiesCli({ cookies: [{ ...cookie, expires: 0 }] }), /refreshToken/);
    await assert.rejects(
      importCookiesCli({ cookies: [{ ...cookie, value: 'x\r\nInjected: bad' }] }),
      { message: rejectedImport },
    );

    const jar = await importCookies({
      cookies: [
        cookie,
        { ...cookie, name: '_analytics' },
        { ...cookie, name: 'id_token', value: 'private-stale', expires: Date.now() / 1000 + 55 },
      ],
    });

    await saveSession(path, jar);
    expect((await stat(path)).mode & 0o777).toBe(0o600);
    expect(await readFile(path, 'utf8')).not.toContain('_analytics');
    let renewals = 0;
    let queries = 0;

    const request = async (url: string, options: RequestInit) => {
      expect(url.startsWith(ORIGIN)).toBe(true);
      // Rust: the adapter sets this; the loopback case checks that no redirect is followed.
      expect(options.redirect).toBe('error');
      const headers = new Headers(options.headers);

      if (url.endsWith('/oauth/token')) {
        renewals++;
        expect(headers.get('cookie')).toContain('refreshToken=');
        const response = Response.json({ access_token: 'private-access' });
        response.headers.append(
          'Set-Cookie',
          `id_token=private-access; Path=/; Max-Age=600; HttpOnly`,
        );
        response.headers.append(
          'Set-Cookie',
          `refreshToken=rotated-${renewals}; Path=/; Max-Age=3600; HttpOnly`,
        );

        return response;
      }

      queries++;
      expect(headers.get('cookie')).toContain('id_token=private-access');
      const stored = await loadSession(path);
      expect((await stored.getCookies(ORIGIN)).find((c) => c.key === 'refreshToken')?.value).toBe(
        `rotated-${renewals}`,
      );

      const body = requestBodySchema.parse(
        await new Request('https://example.test', options).json(),
      );

      if (queries === 1)
        return Response.json({ errors: [{ extensions: { code: 'UNAUTHENTICATED' } }] });

      if (body.operationName === 'Schedule') {
        expect(body.variables.filter).toEqual({
          dateFrom: '2026-09-11',
          dateTo: '2026-09-30',
          label: ['TRAINING'],
        });

        return Response.json({
          data: {
            schedule: {
              edges: [{ node: { ...eventBase, eventId: body.variables.cursor || 'one' } }],
              pageInfo: { hasNextPage: !body.variables.cursor, endCursor: 'opaque-next' },
            },
          },
        });
      }

      return Response.json({ errors: [{ message: 'private-refresh private-access' }] });
    };

    const client = new AblerClient(path, request);
    const filter = { from: '2026-09-11', to: '2026-09-30', types: ['TRAINING' as const], first: 1 };
    const first = await client.schedule(filter);
    assert(first.pageInfo.endCursor);

    const second = await client.schedule({
      ...filter,
      after: first.pageInfo.endCursor,
    });

    expect(first.events[0]?.eventId).toBe('one');
    expect(second.events[0]?.eventId).toBe('opaque-next');
    expect(second.pageInfo.hasNextPage).toBe(false);
    expect(renewals).toBe(2);
    await assert.rejects(client.schedule({ from: '2026-02-30' }));
    await assert.rejects(client.schedule({ from: '2026-09-30', to: '2026-09-11' }));
    await assert.rejects(client.schedule({ first: 101 }));
    // Rust: the tool's input check in place of scheduleInput.parse.
    await assert.rejects(client.schedule({ participantId: 'child-a' }));
    await assert.rejects(client.schedule({ participantIds: [] }));
    await assert.rejects(client.status(), /Abler rejected the request/);
    const stored = await loadSession(path);
    const access = (await stored.getCookies(ORIGIN)).find((c) => c.key === 'id_token');
    assert(access);
    expect(access.TTL()).toBeGreaterThan(500000);
    expect(access.TTL()).toBeLessThanOrEqual(600000);
    expect(access.secure).toBe(true);
    await stored.setCookie('refreshToken=deleted; Path=/; Max-Age=0', ORIGIN);
    await saveSession(path, stored);
    await assert.rejects(loadSession(path), /expired/);
    // Rust: and so does the binary.
    await assert.rejects(client.status(), /expired/);
    await saveSession(path, jar);

    const revoked = new AblerClient(path, async () =>
      Response.json({ error: 'private-refresh' }, { status: 401 }),
    );

    await assert.rejects(revoked.status(), /expired or was revoked/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('AblerClient stops reading API responses after 4 MiB', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-large-response-'));
  const path = join(directory, 'session.json');

  const accessCookie = {
    ...cookie,
    name: 'id_token',
    value: 'private-access',
    expires: Date.now() / 1000 + 3600,
  };

  await saveSession(path, await importCookies([cookie, accessCookie]));
  let chunks = 0;
  let cancelled = false;

  const client = new AblerClient(
    path,
    async () =>
      new Response(
        new ReadableStream<Uint8Array>({
          pull(controller) {
            chunks++;
            controller.enqueue(new Uint8Array(1024 * 1024));
          },
          cancel() {
            cancelled = true;
          },
        }),
      ),
  );

  try {
    await assert.rejects(client.status(), /Abler returned an invalid API response/);
    assert.ok(chunks >= 5);
    assert.equal(cancelled, true);
  } finally {
    await client.close();
    await rm(directory, { recursive: true, force: true });
  }
});

// Rust: 'Chrome capture is limited to loopback and to the Abler tab' calls captureCookies in
// process; tests/ts/capture.test.ts runs it as `auth capture` against the same cdp-mock.

test('groups and event return validated success data, and auth CLI paths stay local', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-targeted-test-'));
  const path = join(directory, 'session.json');

  try {
    const idToken = {
      ...cookie,
      name: 'id_token',
      value: 'private-access',
      expires: Date.now() / 1000 + 3600,
    };

    await saveSession(path, await importCookies({ cookies: [cookie, idToken] }));

    const request = async (_url: string, options: RequestInit) => {
      const body = z
        .object({ operationName: z.string() })
        .parse(await new Request('https://example.test', options).json());

      if (body.operationName === 'Groups')
        return Response.json({
          data: {
            me: {
              userAgeGroups: [
                {
                  id: 'age-group',
                  name: 'U12',
                  isActive: true,
                  sport: { id: 'sport', name: 'Football', unknown: 'removed' },
                  groups: [{ id: 'subgroup', name: 'Blue', label: 'B', unknown: 'removed' }],
                  unknown: 'removed',
                },
              ],
            },
          },
        });

      return Response.json({
        data: {
          event: {
            edges: [
              {
                node: {
                  ...eventBase,
                  eventId: 'event-a',
                  unknown: 'removed',
                },
              },
            ],
            pageInfo: { hasNextPage: false, endCursor: null },
          },
        },
      });
    };

    const client = new AblerClient(path, request);
    assert.deepEqual(await client.groups(), [
      {
        id: 'age-group',
        name: 'U12',
        isActive: true,
        sport: { id: 'sport', name: 'Football' },
        groups: [{ id: 'subgroup', name: 'Blue', label: 'B' }],
      },
    ]);
    assert.deepEqual(await client.event({ eventId: 'event-a', ageGroupId: 'age-group' }), {
      ...eventBase,
      eventId: 'event-a',
    });

    const runCli = async (args: string[]) => {
      const child = await spawnCli(undefined, args, {
        cwd: resolve('.'),
        env: { ...process.env, ABLER_SESSION_FILE: join(directory, 'missing.json') },
        stdout: 'pipe',
        stderr: 'pipe',
      });

      return {
        exit: await child.exited,
        stdout: await new Response(child.stdout).text(),
        stderr: await new Response(child.stderr).text(),
      };
    };

    const status = await runCli(['auth', 'status']);
    assert.equal(status.exit, 1);
    assert.match(status.stderr, /No saved Abler session/);
    const logout = await runCli(['auth', 'logout']);
    assert.equal(logout.exit, 0);
    assert.match(logout.stdout, /failed-import candidates removed/);
    const help = await runCli(['--help']);
    assert.equal(help.exit, 0);
    assert.match(help.stdout, /saved session and failed-import candidates/);
    const capture = await runCli(['auth', 'capture', 'https://example.com']);
    assert.equal(capture.exit, 1);
    assert.match(capture.stderr, /loopback/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('conversations and messages send fixed variables, map null fields, and reject unsafe pages and input', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-messages-'));
  const path = join(directory, 'session.json');

  type JsonFixture =
    | string
    | number
    | boolean
    | null
    | JsonFixture[]
    | { [key: string]: JsonFixture };

  const sentSchema = z.object({
    operationName: z.string(),
    query: z.string(),
    variables: z.record(z.string(), z.json()),
  });

  const sent: z.infer<typeof sentSchema>[] = [];
  let body = '{}';

  const respond = (data: JsonFixture) => {
    body = JSON.stringify({ data });
  };

  const fullMessage = {
    id: 'message-a',
    messageBody: 'Synthetic practice note',
    createdAt: '2026-09-11T16:00:00.000Z',
    creator: { id: 'coach', displayName: 'Coach', unknown: 'removed' },
    attachments: [
      {
        id: 'file-a',
        fileName: 'plan.pdf',
        description: 'Synthetic plan',
        contentType: 'application/pdf',
        path: 'removed',
      },
      { id: 'file-b', fileName: 'map.png', description: null, contentType: 'image/png' },
    ],
    recipient: { isRead: false, unknown: 'removed' },
    unknown: 'removed',
  };

  const sparseMessage = {
    id: 'message-b',
    messageBody: null,
    createdAt: '2026-09-10T08:30:00.000Z',
    creator: null,
    attachments: [],
    recipient: { isRead: null },
  };

  const expectedFull = {
    id: 'message-a',
    body: 'Synthetic practice note',
    createdAt: '2026-09-11T16:00:00.000Z',
    sender: { id: 'coach', displayName: 'Coach' },
    read: false,
    attachments: [
      {
        id: 'file-a',
        fileName: 'plan.pdf',
        description: 'Synthetic plan',
        contentType: 'application/pdf',
      },
      { id: 'file-b', fileName: 'map.png', description: null, contentType: 'image/png' },
    ],
  };

  const expectedSparse = {
    id: 'message-b',
    body: null,
    createdAt: '2026-09-10T08:30:00.000Z',
    sender: null,
    read: null,
    attachments: [],
  };

  try {
    await saveSession(
      path,
      await importCookies([
        cookie,
        {
          ...cookie,
          name: 'id_token',
          value: 'private-access',
          expires: Date.now() / 1000 + 3600,
        },
      ]),
    );

    const client = new AblerClient(path, async (_url: string, options: RequestInit) => {
      sent.push(sentSchema.parse(await new Request('https://example.test', options).json()));

      return new Response(body);
    });

    const conversationPage = {
      getMessageUnreadCount: 3,
      message: {
        edges: [
          {
            node: {
              id: 'conversation-a',
              name: 'Team chat',
              conversationType: 'GROUP_CHAT',
              membersCount: 12,
              unreadCount: 2,
              messageGroup: { id: 'message-group', name: 'U12', unknown: 'removed' },
              user1: { id: 'coach', displayName: 'Coach', unknown: 'removed' },
              user2: { id: 'parent', displayName: 'Parent' },
              messages: { edges: [{ node: fullMessage }] },
              lastMessage: 'removed',
              unknown: 'removed',
            },
          },
          {
            node: {
              id: 'conversation-b',
              name: null,
              conversationType: 'CHAT',
              membersCount: null,
              unreadCount: 0,
              messageGroup: null,
              user1: { id: 'parent', displayName: 'Parent' },
              user2: null,
              messages: { edges: [{ node: sparseMessage }] },
            },
          },
          {
            node: {
              id: 'conversation-c',
              conversationType: 'FUTURE_TYPE',
              unreadCount: 1,
              user1: null,
              messages: { edges: [] },
            },
          },
        ],
        pageInfo: { hasNextPage: true, endCursor: 'cursor-b' },
      },
    };

    respond(conversationPage);
    assert.deepEqual(await client.conversations({ first: 5, after: 'cursor-a' }), {
      unreadCount: 3,
      conversations: [
        {
          id: 'conversation-a',
          name: 'Team chat',
          type: 'GROUP_CHAT',
          membersCount: 12,
          unreadCount: 2,
          group: { id: 'message-group', name: 'U12' },
          participants: [
            { id: 'coach', displayName: 'Coach' },
            { id: 'parent', displayName: 'Parent' },
          ],
          latestMessage: expectedFull,
        },
        {
          id: 'conversation-b',
          name: null,
          type: 'CHAT',
          membersCount: null,
          unreadCount: 0,
          group: null,
          participants: [{ id: 'parent', displayName: 'Parent' }],
          latestMessage: expectedSparse,
        },
        {
          id: 'conversation-c',
          name: null,
          type: 'FUTURE_TYPE',
          membersCount: null,
          unreadCount: 1,
          group: null,
          participants: [],
          latestMessage: null,
        },
      ],
      pageInfo: { hasNextPage: true, endCursor: 'cursor-b' },
    });
    assert.equal(sent.at(-1)?.operationName, 'Conversations');
    assert.deepEqual(sent.at(-1)?.variables, { first: 5, cursor: 'cursor-a' });
    await client.conversations();
    assert.deepEqual(sent.at(-1)?.variables, { first: 20, cursor: null });

    // A missing total is unavailable data, not zero unread messages.
    respond({ ...conversationPage, getMessageUnreadCount: null });
    await assert.rejects(client.conversations());

    respond({
      conversationMessages: {
        edges: [{ node: fullMessage }, { node: { ...sparseMessage, recipient: null } }],
        pageInfo: { hasNextPage: false, endCursor: null },
      },
    });
    assert.deepEqual(await client.messages({ conversationId: 'conversation-a', first: 2 }), {
      messages: [expectedFull, expectedSparse],
      pageInfo: { hasNextPage: false, endCursor: null },
    });
    assert.equal(sent.at(-1)?.operationName, 'ConversationMessages');
    assert.deepEqual(sent.at(-1)?.variables, {
      pagination: { first: 2, after: null },
      conversationIds: ['conversation-a'],
    });
    await client.messages({ conversationId: 'conversation-a', after: 'cursor-a' });
    assert.deepEqual(sent.at(-1)?.variables, {
      pagination: { first: 20, after: 'cursor-a' },
      conversationIds: ['conversation-a'],
    });

    // Neither document asks for attachment URLs, pictures, members, or the null preview field.
    for (const { query } of sent) {
      assert.doesNotMatch(query, /\b(?:path|picture|initials|members|lastMessage)\b/);
    }

    const incomplete = { edges: [], pageInfo: { hasNextPage: true, endCursor: null } };
    respond({ getMessageUnreadCount: 0, message: incomplete, conversationMessages: incomplete });
    // Rust: through MCP this ZodError is toolResult's fixed text, in TypeScript too; the
    // regex matched only the in-process error.
    const incompleteCursor = /Invalid input or unexpected upstream data/;
    await assert.rejects(client.conversations(), incompleteCursor);
    await assert.rejects(client.messages({ conversationId: 'conversation-a' }), incompleteCursor);

    respond({
      getMessageUnreadCount: 0,
      message: {
        edges: [{ node: { id: 'conversation-a', conversationType: 'CHAT', unreadCount: 0 } }],
        pageInfo: { hasNextPage: true, endCursor: 'same' },
      },
      conversationMessages: {
        edges: [{ node: sparseMessage }],
        pageInfo: { hasNextPage: true, endCursor: 'same' },
      },
    });
    await assert.rejects(client.conversations({ after: 'same' }), /did not advance/);
    await assert.rejects(
      client.messages({ conversationId: 'conversation-a', after: 'same' }),
      /did not advance/,
    );

    const requests = sent.length;
    await assert.rejects(client.conversations({ first: 0 }));
    await assert.rejects(client.conversations({ first: 51 }));
    await assert.rejects(client.messages({ conversationId: 'conversation-a', first: 0 }));
    await assert.rejects(client.messages({ conversationId: 'conversation-a', first: 51 }));
    await assert.rejects(client.messages({ conversationId: '' }));
    // Rust: the tools' input checks in place of conversationsInput.parse and messagesInput.parse.
    await assert.rejects(client.conversations({ conversationId: 'conversation-a' }));
    await assert.rejects(client.messages({ conversationId: 'conversation-a', cursor: 'x' }));
    assert.equal(sent.length, requests);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('all eight Abler tools complete MCP round trips with optional and null upstream fields', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-mcp-roundtrip-'));
  const path = join(directory, 'session.json');

  const children = [
    { id: 'child-a', displayName: 'Alex' },
    { id: 'child-b', displayName: 'Jamie' },
  ];

  const event = {
    eventId: 'event-a',
    name: 'Practice',
    description: null,
    from: '2026-09-11T16:00:00Z',
    to: null,
    arrivalTime: 15, // Abler sends minutes before start as a number
    locationDetails: null,
    locationAddress: null,
    locationLink: null,
    ageGroup: { id: 'age-group', name: 'U12' },
    groups: [{ id: 'subgroup', name: 'Blue' }],
    currentPlayerAttendance: children.map((player) => ({
      player,
      status: null,
      coachStatus: null,
    })),
  };

  const message = {
    id: 'message-a',
    messageBody: 'Synthetic practice note',
    createdAt: '2026-09-11T16:00:00.000Z',
    creator: { id: 'coach', displayName: 'Coach' },
    attachments: [
      { id: 'file-a', fileName: 'plan.pdf', description: null, contentType: 'application/pdf' },
    ],
    recipient: { isRead: null },
  };

  const request = async (_url: string, init: RequestInit): Promise<Response> => {
    const { operationName } = z
      .object({ operationName: z.string(), variables: z.record(z.string(), z.unknown()) })
      .parse(await new Request('https://example.test', init).json());

    switch (operationName) {
      case 'SessionStatus':
        return Response.json({ data: { me: { id: 'parent', displayName: 'Parent' } } });
      case 'Profile':
        return Response.json({
          data: { me: { id: 'parent', displayName: 'Parent', children } },
        });
      case 'Groups':
        return Response.json({
          data: {
            me: {
              userAgeGroups: [
                {
                  id: 'age-group',
                  name: 'U12',
                  groups: [{ id: 'subgroup', name: 'Blue', label: null }],
                  sport: null,
                },
              ],
            },
          },
        });
      case 'Schedule':
        return Response.json({
          data: {
            schedule: {
              edges: [{ node: event }],
              pageInfo: { hasNextPage: false, endCursor: null },
            },
          },
        });
      case 'Event':
        return Response.json({
          data: {
            event: {
              edges: [{ node: event }],
              pageInfo: { hasNextPage: false, endCursor: null },
            },
          },
        });
      case 'Conversations':
        return Response.json({
          data: {
            getMessageUnreadCount: 1,
            message: {
              edges: [
                {
                  node: {
                    id: 'conversation-a',
                    name: null,
                    conversationType: 'CHAT',
                    membersCount: 2,
                    unreadCount: 1,
                    messageGroup: null,
                    user1: { id: 'parent', displayName: 'Parent' },
                    user2: null,
                    messages: { edges: [{ node: message }] },
                  },
                },
              ],
              pageInfo: { hasNextPage: false, endCursor: null },
            },
          },
        });
      case 'ConversationMessages':
        return Response.json({
          data: {
            conversationMessages: {
              edges: [{ node: message }],
              pageInfo: { hasNextPage: false, endCursor: null },
            },
          },
        });
      default:
        throw new Error('Unexpected Abler operation.');
    }
  };

  const accessCookie = {
    ...cookie,
    name: 'id_token',
    value: 'private-access',
    expires: Date.now() / 1000 + 3600,
  };

  await saveSession(path, await importCookies([cookie, accessCookie]));

  // Rust: the binary's server over stdio in place of createServer over InMemoryTransport.
  const server = await serveStdio(path, request);
  const client = new Client({ name: 'abler-roundtrip', version: '1.0.0' });

  try {
    await client.connect(server.transport);
    const results = new Map<string, Awaited<ReturnType<typeof client.callTool>>>();

    for (const [name, args] of [
      ['auth_status', {}],
      ['get_profile', {}],
      ['list_groups', {}],
      ['list_schedule', {}],
      ['list_child_schedules', {}],
      ['get_event', { eventId: 'event-a', ageGroupId: 'age-group' }],
      ['list_conversations', {}],
      ['list_messages', { conversationId: 'conversation-a' }],
    ] as const) {
      const result = await client.callTool({ name, arguments: args });
      assert.notEqual(result.isError, true, `${name}: ${JSON.stringify(result.content)}`);
      assert.ok(result.structuredContent, `${name} should return structured output`);
      results.set(name, result);
    }

    assert.equal(results.size, 8);

    const roundTripMessage = {
      id: 'message-a',
      body: 'Synthetic practice note',
      createdAt: '2026-09-11T16:00:00.000Z',
      sender: { id: 'coach', displayName: 'Coach' },
      read: null,
      attachments: [
        { id: 'file-a', fileName: 'plan.pdf', description: null, contentType: 'application/pdf' },
      ],
    };

    assert.deepEqual(results.get('list_conversations')?.structuredContent, {
      unreadCount: 1,
      conversations: [
        {
          id: 'conversation-a',
          name: null,
          type: 'CHAT',
          membersCount: 2,
          unreadCount: 1,
          group: null,
          participants: [{ id: 'parent', displayName: 'Parent' }],
          latestMessage: roundTripMessage,
        },
      ],
      pageInfo: { hasNextPage: false, endCursor: null },
    });
    assert.deepEqual(results.get('list_messages')?.structuredContent, {
      messages: [roundTripMessage],
      pageInfo: { hasNextPage: false, endCursor: null },
    });
    assert.deepEqual(
      z
        .object({ events: z.array(z.object({ description: z.null(), to: z.null() })) })
        .parse(results.get('list_schedule')?.structuredContent).events,
      [{ description: null, to: null }],
    );
    assert.equal(
      z
        .object({ groups: z.array(z.object({ sport: z.null() })) })
        .parse(results.get('list_groups')?.structuredContent).groups[0]?.sport,
      null,
    );
  } finally {
    await client.close();
    await server.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('MCP executable exposes only read tools and reports missing auth without protocol noise', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-mcp-test-'));
  const client = new Client({ name: 'abler-check', version: '1.0.0' });

  const transport = new StdioClientTransport({
    command: rustBinary,
    args: [],
    env: {
      ...process.env,
      ABLER_SESSION_FILE: join(directory, 'missing.json'),
      ABLER_TEST_ORIGIN: NOWHERE,
    },
    stderr: 'pipe',
  });

  try {
    await client.connect(transport);
    const { tools } = await client.listTools();
    expect(tools.map((t) => t.name).toSorted()).toEqual([
      'auth_status',
      'describe_api',
      'get_event',
      'get_profile',
      'list_child_schedules',
      'list_conversations',
      'list_groups',
      'list_messages',
      'list_schedule',
    ]);
    expect(tools.every((tool) => tool.outputSchema)).toBe(true);
    expect(tools.every((t) => t.annotations?.readOnlyHint)).toBe(true);
    const result = await client.callTool({ name: 'auth_status', arguments: {} });
    expect(result.isError).toBe(true);
    expect(JSON.stringify(result)).toContain('No saved Abler session');

    const typo = await client.callTool({
      name: 'list_child_schedules',
      arguments: { childId: 'only-this-child' },
    });

    expect(typo.isError).toBe(true);
    const invalid = await client.callTool({ name: 'list_schedule', arguments: { first: 0 } });
    expect(invalid.isError).toBe(true);

    for (const [name, args] of [
      ['list_conversations', { first: 0 }],
      ['list_conversations', { first: 51 }],
      ['list_conversations', { conversationId: 'conversation-a' }],
      ['list_messages', {}],
      ['list_messages', { conversationId: '' }],
      ['list_messages', { conversationId: 'conversation-a', first: 51 }],
      ['list_messages', { conversationId: 'conversation-a', markRead: true }],
    ] as const) {
      const rejected = await client.callTool({ name, arguments: args });
      expect(rejected.isError).toBe(true);
      expect(JSON.stringify(rejected)).not.toContain('No saved Abler session');
    }
  } finally {
    await client.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('SIGTERM aborts an in-flight Abler fetch and releases its session lock', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-shutdown-'));
  const path = join(directory, 'session.json');
  const preload = join(directory, 'never-fetch.js');
  await saveSession(
    path,
    await importCookies([cookie, { ...cookie, name: 'id_token', value: 'private-access' }]),
  );
  assert.equal(await migrateSession(path), 'migrated');
  const lock = `${store.record}.lock`;
  await writeFile(
    preload,
    `globalThis.fetch = (_input, init) => new Promise((_resolve, reject) => {
      process.stderr.write('FETCH_STARTED\\n');
      const signal = init?.signal;
      const abort = () => reject(new DOMException('Aborted', 'AbortError'));
      if (signal?.aborted) abort();
      else signal?.addEventListener('abort', abort, { once: true });
    });\n`,
    { mode: 0o600 },
  );
  const client = new Client({ name: 'abler-shutdown-test', version: '1.0.0' });
  // Rust: the preload's fetch answers the binary's requests, so its FETCH_STARTED reaches this
  // process instead of the server's stderr.
  const served = await preloadUpstream(preload);

  const transport = new StdioClientTransport({
    command: rustBinary,
    args: [],
    cwd: resolve('.'),
    env: { ...process.env, ABLER_SESSION_FILE: path, ABLER_TEST_ORIGIN: served.origin },
    stderr: 'pipe',
  });

  const fetchStarted = Promise.withResolvers<void>();
  const stopped = Promise.withResolvers<void>();
  onPreloadOutput((text) => {
    if (text.includes('FETCH_STARTED')) fetchStarted.resolve();
  });
  // oxlint-disable-next-line unicorn/prefer-add-event-listener -- The SDK transport exposes only onclose.
  transport.onclose = () => stopped.resolve();

  let pending: Promise<unknown> | undefined;

  try {
    await client.connect(transport);
    const pid = transport.pid;
    assert.ok(pid);
    pending = client.callTool({ name: 'auth_status', arguments: {} });
    void pending.catch(() => {});
    await fetchStarted.promise;
    await stat(lock);
    process.kill(pid, 'SIGTERM');
    await stopped.promise;
    await pending.catch(() => {});
    assert.equal(transport.pid, null);
    await assert.rejects(stat(lock), { code: 'ENOENT' });
    assert.deepEqual(await readdir(directory), ['never-fetch.js']);
  } finally {
    if (transport.pid !== null) process.kill(transport.pid, 'SIGKILL');
    await pending?.catch(() => {});
    await client.close().catch(() => {});
    await served.close();
    await rm(directory, { recursive: true, force: true });
  }
}, 30_000);

test('child schedules separate siblings by ID, retain empty children, and paginate independently', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-children-test-'));
  const path = join(directory, 'session.json');

  const linkedChildren = [
    { id: 'child-a', displayName: 'Alex' },
    { id: 'child-b', displayName: 'Alex' },
    { id: 'child-c', displayName: 'Jamie' },
  ] as const;

  let noChildren = false;
  const requests: { child: string; cursor: string | null }[] = [];

  try {
    await saveSession(
      path,
      await importCookies({ cookies: [cookie, { ...cookie, name: 'id_token' }] }),
    );

    const request = async (_url: string, init: RequestInit) => {
      const body = requestBodySchema.parse(await new Request('https://example.test', init).json());

      if (body.operationName === 'Profile')
        return Response.json({
          data: {
            me: {
              id: 'parent',
              displayName: 'Parent',
              children: noChildren ? [] : linkedChildren,
            },
          },
        });
      expect(body.operationName).toBe('Schedule');
      expect(body.variables.first).toBe(1);
      assert(body.variables.filter?.participant);
      expect(body.variables.filter.participant).toHaveLength(1);
      const child = body.variables.filter.participant[0];
      const cursor = body.variables.cursor;
      assert(child && cursor !== undefined);
      requests.push({ child, cursor });

      return Response.json({
        data: {
          schedule: {
            edges:
              child === 'child-c'
                ? []
                : [
                    {
                      node: {
                        ...eventBase,
                        eventId: cursor ? 'next-event' : 'shared-event',
                        currentPlayerAttendance: [
                          { player: linkedChildren[0], status: 'G', coachStatus: 'P' },
                          { player: linkedChildren[1], status: 'N', coachStatus: null },
                        ],
                      },
                    },
                  ],
            pageInfo: {
              hasNextPage: child === 'child-a' && !cursor,
              endCursor: child === 'child-a' ? 'a-next' : null,
            },
          },
        },
      });
    };

    const client = new AblerClient(path, request);
    expect((await client.profile()).childNamesById).toEqual({
      'child-a': 'Alex',
      'child-b': 'Alex',
      'child-c': 'Jamie',
    });
    const result = await client.childSchedules({ first: 1 });
    assert.deepEqual(
      result.children.map((c) => c.child),
      linkedChildren,
    );
    expect(result.children[0]?.events[0]).toMatchObject({
      eventId: 'shared-event',
      attendance: [{ status: 'G', coachStatus: 'P' }],
    });
    expect(result.children[1]?.events[0]).toMatchObject({
      eventId: 'shared-event',
      attendance: [{ status: 'N', coachStatus: null }],
    });
    expect(result.children[2]).toEqual({
      child: linkedChildren[2],
      events: [],
      pageInfo: { hasNextPage: false, endCursor: null },
    });
    expect(result.children.map((c) => c.pageInfo.hasNextPage)).toEqual([true, false, false]);

    const next = await client.childSchedules({
      childIds: ['child-a'],
      first: 1,
      afterByChild: { 'child-a': 'a-next' },
    });

    expect(next.children).toHaveLength(1);
    expect(next.children[0]?.events[0]?.eventId).toBe('next-event');
    expect(requests).toEqual([
      { child: 'child-a', cursor: null },
      { child: 'child-b', cursor: null },
      { child: 'child-c', cursor: null },
      { child: 'child-a', cursor: 'a-next' },
    ]);
    await assert.rejects(client.childSchedules({ childIds: ['unlinked'] }), /Unknown child/);
    await assert.rejects(
      client.childSchedules({ childIds: ['child-a'], afterByChild: { 'child-b': 'b-next' } }),
      /unselected child/,
    );
    await assert.rejects(client.childSchedules({ from: '2026-09-30', to: '2026-09-11' }));
    // Rust: the tool's input check in place of childSchedulesInput.parse.
    await assert.rejects(client.childSchedules({ childId: 'child-a' }));
    await assert.rejects(client.childSchedules({ participantIds: ['child-a'] }));
    noChildren = true;
    expect(await client.childSchedules()).toEqual({ children: [] });
    expect(requests).toHaveLength(4);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

for (const legacy of [false, true])
  test(`separate processes serialize rotating credentials and logout waits for an in-flight request (${legacy ? 'plaintext file' : 'store'})`, async () => {
    const directory = await mkdtemp(join(tmpdir(), 'abler-concurrency-'));
    const path = join(directory, 'session.json');
    let current = cookie.value;
    let rotations = 0;
    let hold = false;
    const { promise: refreshing, resolve: started } = Promise.withResolvers<void>();
    const { promise: proceed, resolve: release } = Promise.withResolvers<void>();

    const upstream = Bun.serve({
      hostname: '127.0.0.1',
      port: 0,
      async fetch(request) {
        if (new URL(request.url).pathname === '/oauth/token') {
          if (!request.headers.get('cookie')?.includes(`refreshToken=${current}`))
            return new Response(null, { status: 401 });
          current = `rotated-${++rotations}`;

          if (hold) {
            started();
            await proceed;
          }

          const response = Response.json({ access_token: 'access' });
          response.headers.append('Set-Cookie', 'id_token=access; Path=/; Max-Age=600; HttpOnly');
          response.headers.append(
            'Set-Cookie',
            `refreshToken=${current}; Path=/; Max-Age=3600; HttpOnly`,
          );

          return response;
        }

        return Response.json({ data: { me: { id: 'parent', displayName: 'Parent' } } });
      },
    });

    const env = {
      ...process.env,
      ABLER_TEST_FILE: path,
      ABLER_TEST_ORIGIN: `http://127.0.0.1:${upstream.port}`,
    };

    // Rust: `auth status` with a forced refresh, in place of a Bun process that prints
    // `new AblerClient(ABLER_TEST_FILE, request).status(true)`.
    const run = () =>
      Bun.spawn([rustBinary, 'auth', 'status'], {
        env: { ...env, ABLER_SESSION_FILE: path, ABLER_TEST_FORCE_REFRESH: '1' },
        stdout: 'pipe',
        stderr: 'pipe',
      });

    try {
      await saveSession(path, await importCookies([cookie]));

      if (!legacy) assert.equal(await migrateSession(path), 'migrated');
      const processes = [run(), run(), run()];

      const results = await Promise.all(
        processes.map(async (p) => ({
          exit: await p.exited,
          out: await new Response(p.stdout).text(),
          err: await new Response(p.stderr).text(),
        })),
      );

      expect(results.map((r) => r.exit)).toEqual([0, 0, 0]);
      expect(
        results.every(
          (r) => z.object({ authenticated: z.boolean() }).parse(JSON.parse(r.out)).authenticated,
        ),
      ).toBe(true);
      expect(results.every((r) => r.err === '')).toBe(true);
      expect(rotations).toBe(3);
      expect(await (await saved(path)).getCookieString(ORIGIN)).toContain('refreshToken=rotated-3');

      if (legacy) expect(await sessionStorage(path)).toContain('plaintext file');
      else {
        expect(await sessionStorage(path)).toBe('Saved in an encrypted file.');
        expect(
          await filesContaining(['rotated-', 'private-refresh'], directory, store.home),
        ).toEqual([]);
      }

      hold = true;
      const active = run();
      await refreshing;

      // Rust: `auth logout`, which prints only once done. It has started when it holds the session
      // file's lock, which it takes before the store's.
      const logout = Bun.spawn([rustBinary, 'auth', 'logout'], {
        env: { ...env, ABLER_SESSION_FILE: path },
        stdout: 'pipe',
        stderr: 'pipe',
      });

      while (!(await stat(`${path}.lock`).catch(() => undefined))) await Bun.sleep(10);
      expect(logout.exitCode).toBe(null);
      expect(await Bun.file(path).exists()).toBe(legacy);
      release();
      expect(await active.exited).toBe(0);
      expect(await logout.exited).toBe(0);
      expect(await Bun.file(path).exists()).toBe(false);
      expect((await readdir(directory)).length).toBe(0);
      await assert.rejects(saved(path), /No saved Abler session/);
    } finally {
      release();
      await upstream.stop(true);
      await rm(directory, { recursive: true, force: true });
    }
  }, 15000);

test('unsafe session files, malformed pages, stalled cursors, and wrong events fail explicitly', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-validation-'));
  const path = join(directory, 'session.json');

  try {
    await saveSession(path, await importCookies([cookie, { ...cookie, name: 'id_token' }]));

    if (process.platform !== 'win32') {
      // Rust: a read through the binary in place of loadSession.
      const unexpected = () => Promise.reject(new Error('UNEXPECTED_FETCH'));
      await chmod(path, 0o644);
      await assert.rejects(new AblerClient(path, unexpected).status(), /owner-only/);
      await chmod(path, 0o600);
      await symlink(path, join(directory, 'link.json'));
      await assert.rejects(
        new AblerClient(join(directory, 'link.json'), unexpected).status(),
        /symlink/,
      );
    }

    type JsonFixture =
      | string
      | number
      | boolean
      | null
      | JsonFixture[]
      | { [key: string]: JsonFixture };

    type TestPage = {
      edges: { node: { [key: string]: JsonFixture } }[];
      pageInfo: { hasNextPage: boolean; endCursor: string | null };
    };

    let page: TestPage = { edges: [], pageInfo: { hasNextPage: true, endCursor: null } };

    const client = new AblerClient(path, async () =>
      Response.json({ data: { schedule: page, event: page } }),
    );

    await assert.rejects(client.schedule());
    page = { edges: [{ node: {} }], pageInfo: { hasNextPage: false, endCursor: null } };
    await assert.rejects(client.schedule());
    page = {
      edges: [{ node: { ...eventBase, eventId: 'event-a' } }],
      pageInfo: { hasNextPage: true, endCursor: 'same' },
    };
    await assert.rejects(client.schedule({ after: 'same' }), /did not advance/);
    await assert.rejects(
      client.event({ eventId: 'event-b', ageGroupId: 'age-group' }),
      /different event/,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

/** A fake Abler whose refresh rotates to `token`, and whose reads fail unless `ok`. */
const upstream = (token: string, ok: boolean) => `globalThis.fetch = async url => {
  if (url.endsWith('/oauth/token')) {
    const response = Response.json({ access_token: 'access' });
    response.headers.append('Set-Cookie', 'id_token=access; Path=/; Max-Age=600');
    response.headers.append('Set-Cookie', 'refreshToken=${token}; Path=/; Max-Age=3600');
    return response;
  }
  return Response.json(${ok} ? { data: { me: { id: 'parent', displayName: 'Parent' } } } : { errors: [{ message: '${token} secret' }] });
};`;

test('failed import keeps the current session and retains the rotated candidate encrypted until retry-candidate or a verified import', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-import-'));
  const sessions = join(directory, 'sessions');
  await mkdir(sessions);
  const path = join(sessions, 'session.json');
  const source = join(directory, 'cookies.json');
  const preload = join(directory, 'upstream.ts');

  const cli = async (...args: string[]) => {
    const child = await spawnCli(preload, args, {
      env: { ...process.env, ABLER_SESSION_FILE: path },
      stdout: 'pipe',
      stderr: 'pipe',
    });

    const [exit, stdout, stderr] = await Promise.all([
      child.exited,
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
    ]);

    return { exit, stdout, stderr };
  };

  const tokens = ['private-refresh', 'recovery-token', 'verified-token', 'later-token'];

  try {
    // An older version's plaintext session and one of its failed-import candidates.
    await saveSession(path, await importCookies([cookie]));
    await saveSession(`${path}.old.pending`, await importCookies([cookie]));
    await writeFile(source, JSON.stringify([{ ...cookie, value: 'imported-token' }]), {
      mode: 0o600,
    });
    await writeFile(preload, upstream('recovery-token', false));

    const failed = await cli('auth', 'import', source);
    expect(failed.exit).toBe(1);
    expect(failed.stderr.trim()).toBe(
      'Session verification failed. The previous session was kept; the new one is retained in the encrypted store. Run abler-mcp auth retry-candidate, or capture a fresh session.',
    );
    expect(failed.stderr).not.toContain('recovery-token');
    // The plaintext session moved into the store and still works; no .pending file is written.
    expect(await (await saved(path)).getCookieString(ORIGIN)).toContain(
      'refreshToken=private-refresh',
    );
    expect(await (await saved(path, 'candidate')).getCookieString(ORIGIN)).toContain(
      'refreshToken=recovery-token',
    );
    assert.deepEqual(await readdir(sessions), []);
    expect(await filesContaining(tokens, sessions, store.home)).toEqual([]);

    await writeFile(preload, upstream('verified-token', true));
    const retried = await cli('auth', 'retry-candidate');
    expect(retried).toEqual({
      exit: 0,
      stdout: 'Abler session verified and saved in the encrypted store.\n',
      stderr: '',
    });
    expect(await (await saved(path)).getCookieString(ORIGIN)).toContain(
      'refreshToken=verified-token',
    );
    await assert.rejects(saved(path, 'candidate'), /No retained Abler session candidate/);
    expect((await cli('auth', 'retry-candidate')).stderr).toContain(
      'No retained Abler session candidate',
    );

    // A later failed import replaces only the candidate; a verified one promotes its own.
    await writeFile(preload, upstream('recovery-token', false));
    expect((await cli('auth', 'import', source)).exit).toBe(1);
    expect(await (await saved(path)).getCookieString(ORIGIN)).toContain(
      'refreshToken=verified-token',
    );
    await writeFile(preload, upstream('later-token', true));
    const verified = await cli('auth', 'import', source);
    expect(verified.exit).toBe(0);
    expect(verified.stdout).toContain('saved and verified');
    expect(await (await saved(path)).getCookieString(ORIGIN)).toContain('refreshToken=later-token');
    await assert.rejects(saved(path, 'candidate'), /No retained Abler session candidate/);
    expect(await filesContaining(tokens, sessions, store.home)).toEqual([]);

    const status = await cli('auth', 'status');
    expect(status.exit).toBe(0);
    expect(JSON.parse(status.stdout)).toEqual({
      authenticated: true,
      account: { id: 'parent', displayName: 'Parent' },
      storage: 'Saved in an encrypted file.',
    });

    const logout = await cli('auth', 'logout');
    expect(logout.exit).toBe(0);
    expect(logout.stdout).toContain('failed-import candidates removed');
    await assert.rejects(saved(path), /No saved Abler session/);
    assert.deepEqual(await readdir(sessions), []);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

type FakeUpstream = { rotations: number; failRefresh: boolean; seen: string[]; cookies: string[] };

/** A fake Abler that rotates both cookies on every refresh and records what each request sent. */
function rotatingUpstream(before?: () => Promise<number>) {
  const fake: FakeUpstream = {
    rotations: 0,
    failRefresh: false,
    seen: [],
    cookies: [],
  };

  const request = async (url: string, init: RequestInit) => {
    const { pathname } = new URL(url);
    fake.seen.push(before ? `${pathname}@${await before()}` : pathname);
    fake.cookies.push(new Headers(init.headers).get('cookie') ?? '');

    if (pathname === '/oauth/token') {
      const n = ++fake.rotations;

      const response = Response.json(
        { access_token: 'access' },
        { status: fake.failRefresh ? 500 : 200 },
      );

      response.headers.append('Set-Cookie', `id_token=rotated-access-${n}; Path=/; Max-Age=600`);

      response.headers.append(
        'Set-Cookie',
        `refreshToken=rotated-refresh-${n}; Path=/; Max-Age=3600`,
      );

      return response;
    }

    return Response.json({ data: { me: { id: 'parent', displayName: 'Parent' } } });
  };

  return { fake, request };
}

const refreshOf = async (path: string, slot: Slot = 'current') =>
  (await (await saved(path, slot)).getCookies(`${ORIGIN}/`)).find((c) => c.key === 'refreshToken')
    ?.value;

test('capture saves only encrypted cookies; rotations persist before the next request and on error responses', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-rotation-'));
  const path = join(directory, 'session.json');
  const mockChrome = createCdpMock();
  const marker = `${store.record}.marker`;

  const generation = async () =>
    Number(/"generation":(\d+)/.exec(await readFile(marker, 'utf8'))?.[1] ?? Number.NaN);

  const { fake, request } = rotatingUpstream(generation);
  const secrets = ['private-refresh', 'rotated-access', 'rotated-refresh'];

  try {
    // Rust: `auth capture` in place of captureCookies, then saveVerifiedSession.
    expect(
      await captureAndSave(
        `http://127.0.0.1:${mockChrome.server.port}`,
        verifier(request),
        path,
      ),
    ).toBe(false);
    // Generation 1 holds the candidate; its rotation is generation 2 before the read is sent.
    expect(fake.seen).toEqual(['/oauth/token@1', '/graphql@2']);
    expect(fake.cookies[0]).toBe('refreshToken=private-refresh');
    expect(await generation()).toBe(3);
    expect(await refreshOf(path)).toBe('rotated-refresh-1');
    await assert.rejects(saved(path, 'candidate'), /No retained Abler session candidate/);
    expect(await filesContaining(secrets, directory, store.home)).toEqual([]);

    // A refresh that fails upstream still keeps the cookies Abler rotated in its response.
    fake.failRefresh = true;
    const client = new AblerClient(path, request);
    await assert.rejects(client.status(true), /Abler session refresh failed/);
    expect(fake.seen.at(-1)).toBe('/oauth/token@3');
    expect(await generation()).toBe(4);
    expect(await refreshOf(path)).toBe('rotated-refresh-2');
    fake.failRefresh = false;
    expect((await client.status(true)).authenticated).toBe(true);
    expect(fake.cookies.at(-2)).toContain('refreshToken=rotated-refresh-2');
    expect(await filesContaining(secrets, directory, store.home)).toEqual([]);
  } finally {
    await mockChrome.server.stop(true);
    await rm(directory, { recursive: true, force: true });
  }
});

test('migrate moves the plaintext session once, prunes candidates, and the store then decides', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-migrate-'));
  const path = join(directory, 'session.json');
  const { fake, request } = rotatingUpstream();
  const client = new AblerClient(path, request);
  const planted = await importCookies([{ ...cookie, value: 'planted-refresh' }]);

  try {
    await assert.rejects(migrateSession(path), /No saved Abler session/);
    await saveSession(path, await importCookies([cookie]));
    await saveSession(
      `${path}.a.pending`,
      await importCookies([{ ...cookie, value: 'pending-refresh' }]),
    );
    expect(await sessionStorage(path)).toBe(
      'Saved in a plaintext file. Run abler-mcp auth migrate.',
    );

    expect(await migrateSession(path)).toBe('migrated');
    expect(await readdir(directory)).toEqual([]);
    expect(await sessionStorage(path)).toBe('Saved in an encrypted file.');
    expect(await refreshOf(path)).toBe('private-refresh');
    await assert.rejects(saved(path, 'candidate'), /No retained Abler session candidate/);
    expect(await filesContaining(['private-refresh', 'pending-refresh'], store.home)).toEqual([]);
    expect(await migrateSession(path)).toBe('already');

    // Plaintext files planted after migration are never read, and migrate removes them.
    await saveSession(path, planted);
    await saveSession(`${path}.b.pending`, planted);
    expect((await client.status()).authenticated).toBe(true);
    expect(fake.cookies[0]).toBe('refreshToken=private-refresh');
    expect(await migrateSession(path)).toBe('already-removed-legacy');
    expect(await readdir(directory)).toEqual([]);

    // Logout keeps the store deciding: a planted file is still ignored.
    await logoutSession(path);
    await saveSession(path, planted);
    await assert.rejects(client.status(), /No saved Abler session/);
    await assert.rejects(sessionStorage(path), /No saved Abler session/);
    expect(await migrateSession(path)).toBe('already-removed-legacy');
    expect(fake.cookies.join()).not.toContain('planted');
  } finally {
    await client.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('migrate with only failed-import candidates retains the newest readable one for retry-candidate', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-migrate-pending-'));
  const path = join(directory, 'session.json');
  const { fake, request } = rotatingUpstream();
  const verify = verifier(request);

  try {
    await assert.rejects(retryCandidate(verify, path), /No retained Abler session candidate/);
    const old = `${path}.old.pending`;
    await saveSession(old, await importCookies([{ ...cookie, value: 'old-refresh' }]));
    await utimes(old, new Date(Date.now() - 60_000), new Date(Date.now() - 60_000));
    await saveSession(
      `${path}.new.pending`,
      await importCookies([{ ...cookie, value: 'new-refresh' }]),
    );
    await writeFile(`${path}.broken.pending`, 'not a session', { mode: 0o600 });

    expect(await migrateSession(path)).toBe('candidate');
    expect(await readdir(directory)).toEqual([]);
    await assert.rejects(saved(path), /No saved Abler session/);
    expect(await refreshOf(path, 'candidate')).toBe('new-refresh');
    expect(await migrateSession(path)).toBe('already');
    expect(await filesContaining(['old-refresh', 'new-refresh'], store.home)).toEqual([]);

    await retryCandidate(verify, path);
    expect(fake.cookies[0]).toBe('refreshToken=new-refresh');
    expect(await refreshOf(path)).toBe('rotated-refresh-1');
    await assert.rejects(saved(path, 'candidate'), /No retained Abler session candidate/);
    await assert.rejects(retryCandidate(verify, path), /No retained Abler session candidate/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('store failures have fixed messages, change no file and never fall back; only a new import resets a lost key', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-store-errors-'));
  const path = join(directory, 'session.json');
  const keys = new FakeKeyProvider(new Uint8Array(32).fill(7));
  const marker = `${store.record}.marker`;
  const { fake, request } = rotatingUpstream();

  try {
    await saveSession(path, await importCookies([cookie]));
    expect(await migrateSession(path, keys)).toBe('migrated');
    await saveSession(path, await importCookies([{ ...cookie, value: 'planted-refresh' }]));

    const files = async () => [
      await readFile(store.record, 'utf8'),
      await readFile(marker, 'utf8'),
      await readFile(path, 'utf8'),
    ];

    const before = await files();

    // Rust: UNSAFE_FILE, STORE_LOCKED and STORE_BACKEND_RETIRED from an injected provider cannot
    // reach the binary, whose startup check refuses an unsafe store first; src/auth.rs tests
    // their messages, and the next test the retired store.
    for (const [code, message] of [
      ['STORE_UNAVAILABLE', /store key is missing\. Run abler-mcp auth login/],
      ['STORE_ERROR', /damaged, unsafe, or not readable/],
    ] as const) {
      const client = new AblerClient(path, request, failing(code));
      await assert.rejects(client.status(), (error: Error) => {
        expect(error).toBeInstanceOf(SafeError);
        expect(error.message).toMatch(message);

        return true;
      });
      await assert.rejects(statusCli(path, failing(code)), message);
      await assert.rejects(migrateSession(path, failing(code)), message);
      await assert.rejects(
        retryCandidate(async () => ({}), path, failing(code)),
        message,
      );
      await assert.rejects(logoutSession(path, failing(code)), message);
      expect(await files()).toEqual(before);
    }

    expect(fake.seen).toEqual([]);

    // A lost key: reads, migrate and logout refuse; an explicit new import replaces the store.
    const lost = new FakeKeyProvider();
    await assert.rejects(new AblerClient(path, request, lost).status(), /store key is missing/);
    await assert.rejects(logoutSession(path, lost), /store key is missing/);
    expect(await files()).toEqual(before);

    const imported = await importCookies([{ ...cookie, value: 'fresh-refresh' }]);

    expect(
      await saveVerifiedSession(
        imported,
        verifier(request),
        path,
        lost,
      ),
    ).toBe(true);
    expect(fake.cookies[0]).toBe('refreshToken=fresh-refresh');
    expect(await Bun.file(path).exists()).toBe(false);
    expect((await new AblerClient(path, request, lost).status()).authenticated).toBe(true);
    await assert.rejects(new AblerClient(path, request, keys).status(), /damaged|not readable/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('a store left by a Keychain test build is refused before sign-in resets it', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-retired-'));
  const path = join(directory, 'session.json');
  const marker = `${store.record}.marker`;
  const { fake, request } = rotatingUpstream();

  try {
    await mkdir(dirname(store.record), { recursive: true, mode: 0o700 });
    await writeFile(
      marker,
      '{"backend":"encrypted-file","keySource":"keychain-accessor","keyId":"keychain","profile":"default","migrated":true,"generation":1}\n',
      { mode: 0o600 },
    );
    await writeFile(store.record, 'old record', { mode: 0o600 });
    const before = [await readFile(store.record, 'utf8'), await readFile(marker, 'utf8')];
    const retired = /leftover of an earlier test build .* run abler-mcp auth login again\.$/;

    // No key, as that build leaves it: the explicit new sign-in and the import both refuse.
    const lost = new FakeKeyProvider();
    const imported = await importCookies([cookie]);
    // Rust: a verification the binary would run itself; `fake.seen` shows it never ran.
    await assert.rejects(saveVerifiedSession(imported, verifier(request), path, lost), retired);
    await saveSession(path, imported);
    await assert.rejects(migrateSession(path, lost), retired);
    await assert.rejects(new AblerClient(path, request, lost).status(), retired);
    expect([await readFile(store.record, 'utf8'), await readFile(marker, 'utf8')]).toEqual(before);
    expect(fake.seen).toEqual([]);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('ABLER_SESSION_FILE may not overlap the encrypted store or its key', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-collisions-'));
  const path = join(directory, 'session.json');

  try {
    await saveSession(path, await importCookies([cookie]));
    expect(await migrateSession(path)).toBe('migrated');

    const snapshot = async () =>
      Promise.all(
        [store.record, `${store.record}.marker`, store.key].map((file) => readFile(file)),
      );

    const before = await snapshot();
    const jar = await importCookies([cookie]);
    const alias = join(directory, 'alias');
    await symlink(dirname(store.record), alias);

    for (const legacy of [
      store.record,
      `${store.record}.marker`,
      `${store.record}.lock`,
      join(alias, 'session.enc'),
      join(alias, 'session.enc.x.tmp'),
      dirname(store.record),
      store.key,
      `${store.key}.old`,
    ]) {
      for (const run of [
        () => migrateSession(legacy),
        () => logoutSession(legacy),
        () => retryCandidate(async () => ({}), legacy),
        () => saveVerifiedSession(jar, async () => ({}), legacy),
      ])
        await assert.rejects(
          run(),
          /ABLER_SESSION_FILE overlaps the encrypted Abler session store/,
        );
      expect(await snapshot()).toEqual(before);
    }

    expect(await refreshOf(path)).toBe('private-refresh');
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

const uncertain = /The last write to the Abler session store did not complete/;

test('a rotation the store cannot write removes the record, so the spent token is never offered', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-rotation-lost-'));
  const path = join(directory, 'session.json');
  const seen: string[] = [];

  // Abler rotates to a refresh token larger than the store allows.
  const request = async (_url: string, init: RequestInit) => {
    seen.push(new Headers(init.headers).get('cookie') ?? '');
    const response = Response.json({ access_token: 'access' });
    response.headers.append('Set-Cookie', `refreshToken=${'x'.repeat(600_000)}; Path=/`);

    return response;
  };

  try {
    await saveSession(path, await importCookies([cookie]));
    expect(await migrateSession(path)).toBe('migrated');
    const client = new AblerClient(path, request);
    await assert.rejects(client.status(true), uncertain);
    expect(await Bun.file(store.record).exists()).toBe(false);
    await assert.rejects(client.status(true), uncertain);
    expect(seen).toEqual(['refreshToken=private-refresh']);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

const revoked = async () => new Response(null, { status: 401 });

test('verification passes on expired-session and store messages, and never claims a reset store was kept', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'abler-verify-errors-'));
  const path = join(directory, 'session.json');
  const keys = new FakeKeyProvider(new Uint8Array(32).fill(9));

  // Abler refuses the candidate's refresh token.
  const verifyWith = () => verifier(revoked);

  try {
    await saveSession(`${path}.a.pending`, await importCookies([cookie]));
    expect(await migrateSession(path, keys)).toBe('candidate');

    await assert.rejects(retryCandidate(verifyWith(), path, keys), /expired or was revoked/);

    // Rust: 'The candidate replaced between verification and promote is never promoted' swaps
    // the candidate from inside an injected verifier; src/auth.rs tests that promote refuses it.

    // A lost key: a failed import reset the store, so nothing was kept.
    // Rust: a refresh that fails upstream in place of a verifier that throws.
    const lost = new FakeKeyProvider();

    await assert.rejects(
      saveVerifiedSession(
        await importCookies([cookie]),
        verifier(async () => new Response(null, { status: 500 })),
        path,
        lost,
      ),
      (error: Error) => {
        expect(error.message).toContain('was replaced; the new session is retained');
        expect(error.message).not.toContain('previous session was kept');

        return true;
      },
    );

    // Rust: last, in place of a verifier whose store fails with STORE_WRITE_UNCERTAIN: the
    // verification's rotation outgrows the store, which removes the record.
    await assert.rejects(
      retryCandidate(
        verifier(() => {
          const response = Response.json({ access_token: 'access' });
          response.headers.append('Set-Cookie', `refreshToken=${'x'.repeat(600_000)}; Path=/`);

          return response;
        }),
        path,
        lost,
      ),
      uncertain,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
