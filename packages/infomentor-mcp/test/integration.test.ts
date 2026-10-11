import assert from 'node:assert/strict';
import { Server as HttpServer } from 'node:http';
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
  writeFile,
} from 'node:fs/promises';
import { mkdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { spyOn, test } from 'bun:test';
import { setTimeout as delay } from 'node:timers/promises';
import { Client, InMemoryTransport } from '@modelcontextprotocol/client';
import {
  LocalKeyFileProvider,
  SessionStoreError,
  createSecretKey,
  resetSecretStore,
  withSecretStore,
  type KeyProvider,
} from '@family-mcp/session-store';
import { CookieJar } from 'tough-cookie';
import { InfoMentorClient, setupStatusSchema, type SetupStatus } from '../src/client.js';
import { collectionSchema } from '../src/collection.js';
import { PASSWORD_MAX, USERNAME_MAX, readCredentials } from '../src/credentials.js';
import { InfoMentorHttp } from '../src/http.js';
import { authenticate, importSession, login } from '../src/login.js';
import { withSessionLock } from '../src/lock.js';
import { createServer } from '../src/server.js';
import { RECORD_MAX_BYTES, encodeRecord, logout, migrate } from '../src/store.js';
import {
  captureSession,
  InfoMentorError,
  LOGIN_URL,
  MAX_RATE_LIMIT_MS,
  overviewSchema,
  messagesSchema,
  messageSchema,
  notificationsSchema,
  PARENT_URL,
  readSession,
  SESSION_MAX_BYTES,
  sessionStatusSchema,
  writeSession,
} from '../src/session.js';
import {
  anotherHome,
  filesContaining,
  readStored,
  recordOptions,
  useScratchStore,
} from './scratch.js';

const store = useScratchStore();

const credentials = { username: '0101991239', password: 'synthetic-password' };

const parent = {
  account: {
    currentUser: { id: 'parent-1' },
    pupils: [
      { id: 'child-1', name: 'Synthetic child', selected: true },
      { id: 'child-2 & sibling', name: 'Synthetic sibling', selected: false },
    ],
  },
  apps: [{ codeName: 'timetable' }],
};

const entry = {
  start: '2026-09-11T09:00:00',
  end: '2026-09-11T10:00:00',
  title: 'Íslenska',
  startTime: '09:00',
  endTime: '10:00',
  notes: { roomInfo: '', timetableNotes: '', tutors: '' },
  allDay: false,
  establishmentName: 'Synthetic school',
};

const siblingEntry = { ...entry, title: 'Sund' };

const loginHtml =
  '<form method="POST" action="./"><input type="hidden" name="__VIEWSTATE" value="fresh&amp;state"><input type="hidden" name="__EVENTVALIDATION" value="fresh-validation"><input type="hidden" name="__VIEWSTATEGENERATOR" value="generator"></form>';

const relayHtml =
  '<form id="openid_message" method="post" action="https://im1.infomentor.is/Production/Mentor/"><input type="hidden" name="oauth_token" value="synthetic&amp;token"></form>';

const message = {
  id: 41,
  messageContextType: 'General',
  sentUser: { id: 12, displayName: 'Synthetic teacher' },
  isNew: true,
  messageSubject: 'Skólaferð',
  timeSent: '11.09.2026 09:00',
};

const notificationItems = ['New', 'Seen', 'Read', 'Cleared'].map((state, index) => ({
  id: index + 1,
  title: 'Synthetic notification',
  subTitle: 'Bring lunch',
  subjectsCourses: '',
  dateSent: '11.09.2026',
  appType: 'Message',
  state,
  type: 'MessageCreated',
  url: '/#/message/show/41',
  pupilIM2Id: index,
  pupilSourceId: `synthetic-${index}`,
  currentlySelectedPupil: index % 2 === 0,
}));

function fixture() {
  const requests: { url: string; method: string; body: string; cookies: string }[] = [];

  const selection = {
    id: 'child-1',
    overrideUrl: '',
    ignoreSwitch: false,
    failTimetable: false,
    oddItems: false,
    // Called on the parent read, which is the last request before a login or import commits.
    onParent: (): void => {},
  };

  const fetcher = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    assert.ok(input instanceof URL);
    const headers = new Headers(init?.headers);
    const method = init?.method ?? 'GET';
    const body = await new Response(init?.body ?? null).text();
    const cookies = headers.get('cookie') ?? '';
    requests.push({ url: input.href, method, body, cookies });
    assert.equal(init?.redirect, 'manual');

    if (input.href === LOGIN_URL && method === 'GET')
      return new Response(loginHtml, {
        headers: { 'Set-Cookie': 'preflight=synthetic; Secure; HttpOnly; Path=/' },
      });

    if (input.href === LOGIN_URL && method === 'POST') {
      const fields = new URLSearchParams(body);
      assert.equal(fields.get('__VIEWSTATE'), 'fresh&state');
      assert.equal(fields.get('__EVENTVALIDATION'), 'fresh-validation');
      assert.equal(fields.get('login_ascx$txtNotandanafn'), credentials.username);
      assert.equal(fields.get('login_ascx$txtLykilord'), credentials.password);
      assert.equal(fields.get('login_ascx$btnLogin'), 'Innskrá');
      assert.match(cookies, /preflight=synthetic/);
      assert.equal(headers.get('origin'), new URL(LOGIN_URL).origin);

      return new Response(null, {
        status: 302,
        headers: { Location: PARENT_URL + 'authentication/authentication/login' },
      });
    }

    if (input.pathname === '/authentication/authentication/login') {
      assert.equal(method, 'GET');
      assert.equal(cookies, ''); // Host-only cookies cannot leak across the parent/login hosts.

      return new Response(relayHtml);
    }

    if (input.pathname === '/Production/Mentor/') {
      assert.equal(new URLSearchParams(body).get('oauth_token'), 'synthetic&token');
      assert.equal(headers.get('origin'), new URL(PARENT_URL).origin);

      return new Response(null, {
        status: 303,
        headers: {
          Location: PARENT_URL + 'Authentication/Authentication/LoginCallback?token=synthetic',
          'Set-Cookie': '.ASPXAUTH=; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; Secure',
        },
      });
    }

    if (input.pathname.includes('LoginCallback'))
      return new Response(null, {
        status: 302,
        headers: {
          Location: PARENT_URL,
          'Set-Cookie': 'IMHome=synthetic; Secure; HttpOnly; Path=/',
        },
      });

    if (input.pathname.endsWith('/isauthenticated/')) {
      assert.equal(method, 'POST');

      return Response.json(/(?:^|; )IMHome=(?:synthetic|other)(?:;|$)/.test(cookies));
    }

    if (cookies.includes('IMHome=other')) {
      if (input.href === PARENT_URL) {
        const model = {
          ...parent,
          account: {
            currentUser: { id: 'parent-2' },
            pupils: [
              {
                id: 'only-child',
                name: 'Another account child',
                selected: true,
                switchPupilUrl: null,
              },
            ],
          },
        };

        return new Response(
          `<script>IMHome.home.homeData = ${JSON.stringify(model)}; IMHome.home.init(IMHome.home.homeData);</script>`,
        );
      }

      if (input.pathname === '/timetable/timetable/appData')
        return Response.json({ items: [{ ...entry, title: 'Another account timetable' }] });
      throw new Error('Unexpected request in the other account');
    }

    if (input.href === PARENT_URL) {
      selection.onParent();

      const model = {
        ...parent,
        account: {
          currentUser: { id: 'parent-1' },
          pupils: parent.account.pupils.map((pupil, index) => ({
            ...pupil,
            selected: pupil.id === selection.id,
            switchPupilUrl:
              selection.overrideUrl || `/Account/PupilSwitcher/SwitchPupil/${101 + index}`,
          })),
        },
      };

      return new Response(
        `<script>IMHome.home.homeData = ${JSON.stringify(model)}; IMHome.home.init(IMHome.home.homeData);</script>`,
      );
    }

    if (/^\/Account\/PupilSwitcher\/SwitchPupil\/(101|102)$/.test(input.pathname)) {
      assert.equal(method, 'GET');
      const pupil = parent.account.pupils[Number(input.pathname.split('/').at(-1)) - 101];
      assert.ok(pupil);

      if (!selection.ignoreSwitch) selection.id = pupil.id;

      return new Response(null, {
        status: 302,
        headers: {
          Location: PARENT_URL,
          'Set-Cookie': `selectedChild=${encodeURIComponent(selection.id)}; Secure; HttpOnly; Path=/`,
        },
      });
    }

    if (input.pathname === '/timetable/timetable/appData') {
      assert.equal(method, 'POST');
      assert.match(cookies, /IMHome=synthetic/);

      if (selection.failTimetable) return new Response('private-upstream-value', { status: 500 });

      if (selection.id !== 'child-1')
        assert.ok(cookies.includes(`selectedChild=${encodeURIComponent(selection.id)}`));

      const timetableEntry = selection.id === 'child-1' ? entry : siblingEntry;

      return Response.json({
        items: selection.oddItems
          ? [
              { ...timetableEntry, establishmentName: null },
              { ...timetableEntry, title: null },
            ]
          : [timetableEntry],
      });
    }

    if (input.pathname === '/Message/message/GetMessages') {
      assert.equal(method, 'POST');
      assert.match(cookies, /IMHome=synthetic/);
      const fields = new URLSearchParams(body);

      if (fields.get('messageText') === 'malformed')
        return new Response('{"items":"private-upstream-value"}');

      if (fields.get('messageText') === '') {
        assert.equal(fields.get('pageSize'), '100');
        assert.equal(fields.get('page'), '1');

        const messageItem = selection.oddItems
          ? { ...message, sentUser: { ...message.sentUser, displayName: null } }
          : message;

        const items: unknown[] = fields.get('inbox') === 'true' ? [messageItem] : [];

        if (selection.oddItems) items.push({ ...messageItem, id: 'invalid' });

        return Response.json({
          items,
          more: false,
        });
      }

      assert.equal(fields.get('inbox'), 'false');
      assert.equal(fields.get('sentItems'), 'true');
      assert.equal(fields.get('messageText'), 'Skólaferð & nesti');
      assert.equal(fields.get('page'), '2');
      assert.equal(fields.get('pageSize'), '1');

      const messageItem = selection.oddItems
        ? { ...message, sentUser: { ...message.sentUser, displayName: null } }
        : message;

      const items: unknown[] = [messageItem];

      if (selection.oddItems) items.push({ ...messageItem, id: 'invalid' });

      return Response.json({ items, page: 0, more: true });
    }

    if (input.pathname === '/Message/message/GetMessage') {
      assert.equal(method, 'POST');
      assert.equal(new URLSearchParams(body).get('id'), '41');

      return Response.json({
        ...message,
        sentUser: selection.oddItems
          ? { ...message.sentUser, displayName: null }
          : message.sentUser,
        messageBody: '<p>Bring lunch</p>',
        messageBodyPlainText: 'Bring lunch',
        toUsers: [
          {
            id: 13,
            displayName: selection.oddItems ? null : 'Synthetic parent',
          },
        ],
        messageFolder: 'Inbox',
      });
    }

    if (input.pathname === '/NotificationApp/NotificationApp/appData') {
      assert.equal(method, 'POST');

      const feed: unknown[] = notificationItems.map((item) =>
        Object.assign({}, item, {
          currentlySelectedPupil:
            selection.id === 'child-1' ? item.currentlySelectedPupil : !item.currentlySelectedPupil,
        }),
      );

      if (selection.oddItems) {
        const first = notificationItems[0];
        assert.ok(first);
        feed.push({ ...first, id: 5, state: 'FutureState' });
        feed.push({ ...first, id: 'invalid' });
      }

      return Response.json({ notifications: feed });
    }

    throw new Error('Unexpected synthetic endpoint');
  };

  return { requests, selection, fetch: fetcher, restore: () => {} };
}

const unsafe = (pattern: RegExp) => (cause: unknown) =>
  cause instanceof InfoMentorError &&
  cause.code === 'INVALID_SESSION' &&
  pattern.test(cause.message);

const mismatch = (cause: unknown) =>
  cause instanceof InfoMentorError && /different InfoMentor account/.test(cause.message);

const limited =
  (minimumMs: number, maximumMs: number) =>
  (cause: unknown): boolean =>
    cause instanceof InfoMentorError &&
    cause.code === 'RATE_LIMITED' &&
    cause.retryAfterMs !== undefined &&
    cause.retryAfterMs > minimumMs &&
    cause.retryAfterMs <= maximumMs;

async function savedSession(value = 'synthetic') {
  const jar = new CookieJar();
  await jar.setCookie(`IMHome=${value}; Secure; HttpOnly; Path=/`, PARENT_URL);

  return captureSession(jar);
}

test('private login and eleven MCP tools select children and read school data without changing read state', async () => {
  // The four setup tools are an opt-in; this test enables them to drive login and logout over MCP.
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-http-'));
  const file = join(directory, 'private/session.json');

  const environment = {
    INFOMENTOR_USERNAME: process.env['INFOMENTOR_USERNAME'],
    INFOMENTOR_PASSWORD: process.env['INFOMENTOR_PASSWORD'],
    INFOMENTOR_CREDENTIALS_FILE: process.env['INFOMENTOR_CREDENTIALS_FILE'],
  };

  const routes = fixture();

  const listen = spyOn(HttpServer.prototype, 'listen').mockImplementation(() =>
    assert.fail('Private login must never open a credential listener.'),
  );

  const server = createServer({ sessionFile: file, allowSetupTools: true, fetch: routes.fetch });
  const client = new Client({ name: 'http-test', version: '1.0.0' });
  const [serverTransport, clientTransport] = InMemoryTransport.createLinkedPair();

  try {
    for (const name of Object.keys(environment)) delete process.env[name];

    // By default the server registers only the seven read and collection tools, and a missing
    // session points the user at the CLI.
    const readOnlyServer = createServer({ sessionFile: file, fetch: routes.fetch });
    const readOnlyClient = new Client({ name: 'default-test', version: '1.0.0' });
    const [readOnlyServerTransport, readOnlyClientTransport] = InMemoryTransport.createLinkedPair();
    await readOnlyServer.connect(readOnlyServerTransport);
    await readOnlyClient.connect(readOnlyClientTransport);

    try {
      assert.deepEqual(
        (await readOnlyClient.listTools()).tools.map((tool) => tool.name).toSorted(),
        [
          'infomentor_collect_updates',
          'infomentor_get_message',
          'infomentor_get_messages',
          'infomentor_get_notifications',
          'infomentor_get_overview',
          'infomentor_select_child',
          'infomentor_session_status',
        ],
      );

      const missing = sessionStatusSchema.parse(
        (await readOnlyClient.callTool({ name: 'infomentor_session_status', arguments: {} }))
          .structuredContent,
      );

      assert.equal(missing.authenticated, false);
      assert.match(missing.nextStep ?? '', /infomentor-mcp login/);

      for (const name of ['infomentor_login', 'infomentor_logout']) {
        const attempt = await readOnlyClient
          .callTool({ name, arguments: {} })
          .catch(() => ({ isError: true }));

        assert.equal(attempt.isError, true);
      }

      assert.equal(routes.requests.length, 0);
    } finally {
      await readOnlyClient.close();
      await readOnlyServer.close();
    }

    await assert.rejects(login({ sessionFile: file, timeoutMs: 30_000, fetch: routes.fetch }), {
      code: 'INVALID_CONFIGURATION',
    });
    assert.equal(routes.requests.length, 0);
    process.env['INFOMENTOR_USERNAME'] = credentials.username;
    await assert.rejects(login({ sessionFile: file, timeoutMs: 30_000, fetch: routes.fetch }), {
      code: 'INVALID_CONFIGURATION',
    });
    assert.equal(routes.requests.length, 0);
    delete process.env['INFOMENTOR_USERNAME'];
    await server.connect(serverTransport);
    await client.connect(clientTransport);
    const tools = (await client.listTools()).tools;
    assert.equal(tools.length, 11);
    assert.ok(tools.every((tool) => tool.outputSchema));
    assert.ok(!tools.some((tool) => tool.name.includes('browser')));
    const loginTool = tools.find((tool) => tool.name === 'infomentor_login');
    assert.ok(loginTool);
    assert.equal(loginTool.inputSchema.additionalProperties, false);
    assert.ok(!('localForm' in (loginTool.inputSchema.properties ?? {})));

    for (const localForm of [true, false]) {
      const rejected = await client.callTool({
        name: 'infomentor_login',
        arguments: { localForm },
      });

      assert.equal(rejected.isError, true);
      assert.match(JSON.stringify(rejected), /localForm/);
      const status = await client.callTool({ name: 'infomentor_setup_status', arguments: {} });
      assert.equal(setupStatusSchema.parse(status.structuredContent).state, 'idle');
      assert.equal(routes.requests.length, 0);
      assert.equal(listen.mock.calls.length, 0);
    }

    process.env['INFOMENTOR_USERNAME'] = credentials.username;
    process.env['INFOMENTOR_PASSWORD'] = credentials.password;

    const started = await client.callTool({
      name: 'infomentor_login',
      arguments: {},
    });

    assert.equal(setupStatusSchema.parse(started.structuredContent).state, 'running');
    let state = 'running';

    for (let step = 0; step < 3_000 && state === 'running'; step++) {
      await delay(10);
      const status = await client.callTool({ name: 'infomentor_setup_status', arguments: {} });
      const progress = setupStatusSchema.parse(status.structuredContent);
      assert.ok(!('loginUrl' in progress));
      assert.equal(JSON.stringify(status).includes('loginUrl'), false);
      assert.equal(JSON.stringify(status).includes(credentials.password), false);
      state = progress.state;
    }

    assert.equal(state, 'succeeded');
    assert.equal(listen.mock.calls.length, 0);
    const record = await readStored(store);
    const stored = record.session;
    assert.ok(stored);
    assert.equal(stored.version, 2);
    assert.ok(stored.cookies.some((cookie) => cookie.key === 'IMHome'));
    assert.ok(stored.cookies.every((cookie) => cookie.value && cookie.key !== '.ASPXAUTH'));
    // The injected sign-in is stored with the session, and nothing is left in plaintext.
    assert.deepEqual(record.credentials, credentials);
    await assert.rejects(stat(file), { code: 'ENOENT' });
    assert.deepEqual(
      await filesContaining([credentials.username, credentials.password], directory, store.home),
      [],
    );

    if (process.platform !== 'win32') assert.equal((await stat(store.record)).mode & 0o777, 0o600);
    const beforeOverview: number = routes.requests.length;
    const result = await client.callTool({ name: 'infomentor_get_overview', arguments: {} });
    const overview = overviewSchema.parse(result.structuredContent);
    assert.deepEqual(overview.children, parent.account.pupils);
    assert.deepEqual(overview.timetable, [entry]);
    assert.match(overview.text, /Íslenska/);
    assert.deepEqual(
      routes.requests.slice(beforeOverview).map(({ url }) => url),
      [PARENT_URL, PARENT_URL + 'timetable/timetable/appData'],
    );

    const messagesResult = await client.callTool({
      name: 'infomentor_get_messages',
      arguments: { folder: 'sent', search: 'Skólaferð & nesti', page: 2, pageSize: 1 },
    });

    const listed = messagesSchema.parse(messagesResult.structuredContent);
    assert.deepEqual(listed.items, [message]);
    assert.equal(listed.page, 2);
    assert.equal(listed.more, true);

    const detailResult = await client.callTool({
      name: 'infomentor_get_message',
      arguments: { id: 41 },
    });

    const detail = messageSchema.parse(detailResult.structuredContent).message;
    assert.equal(detail.messageBodyPlainText, 'Bring lunch');
    assert.equal(detail.isNew, true);
    assert.ok(!('messageBody' in detail));

    for (const [arguments_, expectedIds] of [
      [{}, [1, 2, 3]],
      [{ selectedChildOnly: true }, [1, 3]],
      [{ includeCleared: true }, [1, 2, 3, 4]],
    ] as const) {
      const feed = await client.callTool({
        name: 'infomentor_get_notifications',
        arguments: arguments_,
      });

      assert.deepEqual(
        notificationsSchema.parse(feed.structuredContent).notifications.map((item) => item.id),
        expectedIds,
      );
    }

    const beforeInvalid = routes.requests.length;
    assert.equal(
      (await client.callTool({ name: 'infomentor_get_message', arguments: { id: -1 } })).isError,
      true,
    );
    assert.equal(
      (await client.callTool({ name: 'infomentor_get_messages', arguments: { pageSize: 101 } }))
        .isError,
      true,
    );
    assert.equal(
      (await client.callTool({ name: 'infomentor_select_child', arguments: { childId: '' } }))
        .isError,
      true,
    );
    assert.equal(routes.requests.length, beforeInvalid);

    const switching = client.callTool({
      name: 'infomentor_select_child',
      arguments: { childId: 'child-2 & sibling' },
    });

    const queuedOverview = client.callTool({ name: 'infomentor_get_overview', arguments: {} });

    for (const response of await Promise.all([switching, queuedOverview])) {
      const switched = overviewSchema.parse(response.structuredContent);
      assert.equal(switched.children.find((child) => child.selected)?.id, 'child-2 & sibling');
      assert.deepEqual(switched.timetable, [siblingEntry]);
      assert.ok(!JSON.stringify(response).includes('switchPupilUrl'));
    }

    const switchRequests = () => routes.requests.filter(({ url }) => url.includes('/SwitchPupil/'));

    const noOp = await client.callTool({
      name: 'infomentor_select_child',
      arguments: { childId: 'child-2 & sibling' },
    });

    assert.notEqual(noOp.isError, true);
    assert.equal(switchRequests().length, 1);

    const siblingFeed = await client.callTool({
      name: 'infomentor_get_notifications',
      arguments: { selectedChildOnly: true },
    });

    assert.deepEqual(
      notificationsSchema.parse(siblingFeed.structuredContent).notifications.map(({ id }) => id),
      [2],
    );

    const siblingMessages = await client.callTool({
      name: 'infomentor_get_messages',
      arguments: { folder: 'sent', search: 'Skólaferð & nesti', page: 2, pageSize: 1 },
    });

    assert.deepEqual(messagesSchema.parse(siblingMessages.structuredContent).items, [message]);

    const unknownChild = await client.callTool({
      name: 'infomentor_select_child',
      arguments: { childId: 'not-registered' },
    });

    assert.equal(unknownChild.isError, true);
    assert.equal(unknownChild.structuredContent, undefined);
    assert.equal(switchRequests().length, 1);

    for (const url of [
      'https://evil.test/Account/PupilSwitcher/SwitchPupil/101',
      'https://im1.infomentor.is/Account/PupilSwitcher/SwitchPupil/101',
      '/Message/message/DeleteMessage/101',
      '/Account/PupilSwitcher/SwitchPupil/101?private=synthetic',
    ]) {
      routes.selection.overrideUrl = url;
      const beforeRejectedUrl: number = routes.requests.length;

      const refused = await client.callTool({
        name: 'infomentor_select_child',
        arguments: { childId: 'child-1' },
      });

      assert.equal(refused.isError, true);
      assert.ok(!JSON.stringify(refused).includes(url));
      assert.equal(switchRequests().length, 1);
      assert.deepEqual(
        routes.requests.slice(beforeRejectedUrl).map(({ url: requested }) => requested),
        [PARENT_URL, PARENT_URL],
      );
    }

    routes.selection.overrideUrl = '';
    routes.selection.ignoreSwitch = true;

    const unconfirmed = await client.callTool({
      name: 'infomentor_select_child',
      arguments: { childId: 'child-1' },
    });

    assert.equal(unconfirmed.isError, true);
    assert.equal(routes.selection.id, 'child-2 & sibling');
    routes.selection.ignoreSwitch = false;

    const restored = await client.callTool({
      name: 'infomentor_select_child',
      arguments: { childId: 'child-1' },
    });

    assert.deepEqual(
      overviewSchema.parse(restored.structuredContent).children,
      parent.account.pupils,
    );
    assert.equal(
      tools.find(({ name }) => name === 'infomentor_select_child')?.annotations?.readOnlyHint,
      false,
    );

    routes.selection.failTimetable = true;

    const partial = await client.callTool({
      name: 'infomentor_select_child',
      arguments: { childId: 'child-2 & sibling' },
    });

    assert.equal(partial.isError, true);
    assert.match(JSON.stringify(partial), /Selection may have changed/);
    assert.ok(!JSON.stringify(partial).includes('private-upstream-value'));
    assert.equal(routes.selection.id, 'child-2 & sibling');
    routes.selection.failTimetable = false;

    // Another account's older plaintext session, before any store exists for it.
    const restoreHome = await anotherHome(store);
    const otherFile = join(directory, 'other/session.json');
    await writeSession(await savedSession('other'), otherFile);
    const otherClient = new InfoMentorClient({ sessionFile: otherFile, fetch: routes.fetch });

    try {
      const onlyChild = await otherClient.selectChild({ childId: 'only-child' });
      assert.deepEqual(onlyChild.children, [
        { id: 'only-child', name: 'Another account child', selected: true },
      ]);
      assert.equal(onlyChild.timetable?.[0]?.title, 'Another account timetable');
      await assert.rejects(otherClient.selectChild({ childId: 'child-2 & sibling' }), {
        code: 'INVALID_CONFIGURATION',
      });
      assert.equal((await otherClient.getOverview()).children[0]?.id, 'only-child');
      assert.equal(routes.selection.id, 'child-2 & sibling');
    } finally {
      await otherClient.close();
      restoreHome();
    }

    await client.callTool({ name: 'infomentor_select_child', arguments: { childId: 'child-1' } });

    const malformed = await client.callTool({
      name: 'infomentor_get_messages',
      arguments: { search: 'malformed' },
    });

    assert.equal(malformed.isError, true);
    assert.ok(!JSON.stringify(malformed).includes('private-upstream-value'));
    assert.ok(
      !routes.requests.some(({ url }) =>
        /ViewedMessage|UpdateNotificationState|SendMessage|DeleteMessage/i.test(url),
      ),
    );
    assert.ok(
      tools
        .filter((tool) =>
          [
            'infomentor_get_messages',
            'infomentor_get_message',
            'infomentor_get_notifications',
          ].includes(tool.name),
        )
        .every((tool) => tool.annotations?.readOnlyHint),
    );
    const freshClient = new InfoMentorClient({ sessionFile: file, fetch: routes.fetch });

    try {
      assert.equal((await freshClient.getSessionStatus()).authenticated, true);
    } finally {
      await freshClient.close();
    }

    const baselineResult = await client.callTool({
      name: 'infomentor_collect_updates',
      arguments: {},
    });

    assert.equal(baselineResult.isError, undefined);
    const baseline = collectionSchema.parse(baselineResult.structuredContent);
    assert.equal(baseline.baseline, true);
    assert.deepEqual(baseline.updates, []);
    assert.equal(baseline.children.length, 2);

    const unchanged = collectionSchema.parse(
      (
        await client.callTool({
          name: 'infomentor_collect_updates',
          arguments: { cursor: baseline.cursor },
        })
      ).structuredContent,
    );

    assert.equal(unchanged.cursor, baseline.cursor);
    assert.deepEqual(unchanged.updates, []);
    assert.equal(routes.selection.id, 'child-1');

    await client.callTool({ name: 'infomentor_logout', arguments: {} });
    assert.equal(
      sessionStatusSchema.parse(
        (await client.callTool({ name: 'infomentor_session_status', arguments: {} }))
          .structuredContent,
      ).authenticated,
      false,
    );
    assert.equal(
      routes.requests.filter((request) => request.body.includes('txtLykilord')).length,
      1,
    );
  } finally {
    await client.close();
    await server.close();
    routes.restore();
    listen.mockRestore();

    for (const [name, value] of Object.entries(environment)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }

    await rm(directory, { recursive: true, force: true });
  }
});

test('InfoMentor skips malformed feed items and preserves nullable and unknown values', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-tolerant-feeds-'));
  const file = join(directory, 'session.json');
  await writeSession(await savedSession(), file);
  const routes = fixture();
  routes.selection.oddItems = true;
  const client = new InfoMentorClient({ sessionFile: file, fetch: routes.fetch });

  try {
    const overview = await client.getOverview();
    assert.equal(overview.skipped, 1);
    assert.equal(overview.timetable?.[0]?.establishmentName, null);

    const messages = await client.getMessages({
      folder: 'sent',
      search: 'Skólaferð & nesti',
      page: 2,
      pageSize: 1,
    });

    assert.equal(messages.skipped, 1);
    assert.equal(messages.items[0]?.sentUser.displayName, null);

    const detail = await client.getMessage({ id: 41 });
    assert.equal(detail.message.sentUser.displayName, null);
    assert.equal(detail.message.toUsers[0]?.displayName, null);

    const notificationResult = await client.getNotifications({ includeCleared: true });
    assert.equal(notificationResult.skipped, 1);
    assert.equal(
      notificationResult.notifications.find((item) => item.id === 5)?.state,
      'FutureState',
    );

    const collection = await client.collectUpdates({ includeExisting: true });
    assert.equal(collection.skipped, 8);
    assert.deepEqual(collection.skippedByFeed, {
      timetable: 2,
      messages: 4,
      notifications: 2,
    });
    assert.deepEqual(collection.missing, []);
    assert.ok(collection.updates.every((update) => update.kind === 'child'));
  } finally {
    await client.close();
    routes.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('expired sessions renew once with private credentials, preserve account and child, and persist cookies', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-refresh-'));
  const file = join(directory, 'session.json');
  const credentialsFile = join(directory, 'credentials.json');
  await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });
  const routes = fixture();
  let expired = false;
  let redirectParent = false;
  let expireMessage = false;
  let rejectPassword = false;
  let wrongAccount = false;
  let failureStatus = 0;
  let passwordSubmissions = 0;

  const fetcher = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    assert.ok(input instanceof URL);
    const body = await new Response(init?.body ?? null).text();

    if (body.includes('txtLykilord')) {
      passwordSubmissions++;

      if (rejectPassword) return new Response(loginHtml);
      expired = false;
      redirectParent = false;
      routes.selection.id = 'child-1';
    }

    if (failureStatus && input.href === PARENT_URL)
      return new Response('', { status: failureStatus });

    if (input.pathname.endsWith('/isauthenticated/')) {
      if (expired) return Response.json(false);
    }

    if (expired && input.href === PARENT_URL) return new Response('', { status: 401 });

    if (redirectParent && input.href === PARENT_URL)
      return new Response(null, {
        status: 302,
        headers: { Location: PARENT_URL + 'authentication/authentication/login' },
      });

    if (redirectParent && input.pathname === '/authentication/authentication/login')
      return new Response(relayHtml);

    if (input.pathname === '/Message/message/GetMessage' && expireMessage) {
      expireMessage = false;
      expired = true;

      return new Response('', { status: 401 });
    }

    const response = await routes.fetch(input, init);

    if (input.href === PARENT_URL && wrongAccount)
      return new Response((await response.text()).replace('parent-1', 'different-parent'));

    if (input.pathname === '/Message/message/GetMessage')
      response.headers.append('Set-Cookie', 'rotation=kept; Secure; HttpOnly; Path=/');

    return response;
  };

  let client = new InfoMentorClient({ sessionFile: file, credentialsFile, fetch: fetcher });

  try {
    // A missing/deleted session needs explicit login, even when credentials are configured.
    assert.equal((await client.getSessionStatus()).authenticated, false);
    assert.equal(passwordSubmissions, 0);
    assert.equal(
      await login({ sessionFile: file, credentialsFile, fetch: fetcher }),
      credentialsFile,
    );
    assert.equal((await readStored(store)).session?.accountId, 'parent-1');
    assert.deepEqual((await readStored(store)).credentials, credentials);
    await client.selectChild({ childId: 'child-2 & sibling' });
    expired = true;
    const beforeRefresh = passwordSubmissions;
    const overviews = await Promise.all([client.getOverview(), client.getOverview()]);
    assert.equal(passwordSubmissions, beforeRefresh + 1);
    assert.ok(
      overviews.every(
        (overview) => overview.children.find((child) => child.selected)?.id === 'child-2 & sibling',
      ),
    );
    assert.deepEqual(overviews[0].timetable, [siblingEntry]);
    expireMessage = true;
    const bodyBefore = passwordSubmissions;
    assert.equal((await client.getMessage({ id: 41 })).message.messageBodyPlainText, 'Bring lunch');
    assert.equal(passwordSubmissions, bodyBefore + 1, 'mid-read expiry gets exactly one recovery');
    assert.ok(
      (await readStored(store)).session?.cookies.some(
        (cookie) => cookie.key === 'rotation' && cookie.value === 'kept',
      ),
    );
    expireMessage = true;
    const collectionBefore = passwordSubmissions;
    const collected = await client.collectUpdates({ includeExisting: true });
    assert.equal(passwordSubmissions, collectionBefore + 1);
    assert.equal(
      routes.selection.id,
      'child-2 & sibling',
      'mid-collection expiry restores the pre-collection child',
    );
    assert.ok(
      collected.updates.some((update) => update.kind === 'message' && update.childIds.length === 2),
    );
    await client.close();
    client = new InfoMentorClient({ sessionFile: file, credentialsFile, fetch: fetcher });
    const restartBefore = passwordSubmissions;
    assert.equal((await client.getSessionStatus()).authenticated, true);
    assert.equal(
      passwordSubmissions,
      restartBefore,
      'a new process reuses persisted renewed cookies',
    );
    redirectParent = true;
    const redirectBefore = passwordSubmissions;
    await client.getOverview();
    assert.equal(
      passwordSubmissions,
      redirectBefore + 1,
      'a known login redirect renews even when isauthenticated returned true',
    );

    for (const status of [403, 500]) {
      failureStatus = status;
      const count: number = passwordSubmissions;
      await assert.rejects(client.getOverview());
      assert.equal(
        passwordSubmissions,
        count,
        'non-authentication failures never submit credentials',
      );
      await client.close();
      client = new InfoMentorClient({ sessionFile: file, credentialsFile, fetch: fetcher });
    }

    failureStatus = 0;

    for (const mode of ['rejected', 'wrong-account']) {
      expired = true;
      rejectPassword = mode === 'rejected';
      wrongAccount = mode === 'wrong-account';
      const before = await readFile(store.record, 'utf8');
      const count: number = passwordSubmissions;
      await assert.rejects(client.getOverview(), { code: 'LOGIN_REQUIRED' });
      assert.equal(passwordSubmissions, count + 1, 'a rejected sign-in is never retried');
      assert.equal(
        await readFile(store.record, 'utf8'),
        before,
        'failed recovery must preserve the prior session',
      );
    }

    rejectPassword = false;
    wrongAccount = false;

    // Without a credentials file or environment, the stored sign-in renews the session.
    await client.close();
    await rm(credentialsFile);
    client = new InfoMentorClient({ sessionFile: file, fetch: fetcher });
    expired = true;
    const storedBefore = passwordSubmissions;
    await client.getOverview();
    assert.equal(passwordSubmissions, storedBefore + 1);
    const status = await client.getSessionStatus();
    assert.match(
      status.storage ?? '',
      /^Saved in an encrypted file\. Your InfoMentor sign-in is stored/,
    );
    assert.ok(!JSON.stringify(status).includes(credentials.password));
    assert.deepEqual(
      await filesContaining([credentials.username, credentials.password], directory, store.home),
      [],
    );

    expired = false;
    await client.logout();
    assert.deepEqual(await readStored(store), { version: 1, session: null, credentials: null });
    assert.equal((await client.getSessionStatus()).authenticated, false);
    await client.close();
    // An older plaintext session, before any store exists.
    await anotherHome(store);
    await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });
    routes.selection.id = 'child-1';
    const legacyFile = join(directory, 'legacy.json');
    await writeSession(await savedSession(), legacyFile);
    client = new InfoMentorClient({ sessionFile: legacyFile, credentialsFile, fetch: fetcher });
    expired = true;
    const legacyCount = passwordSubmissions;
    await assert.rejects(client.getOverview(), { code: 'LOGIN_REQUIRED' });
    assert.equal(
      passwordSubmissions,
      legacyCount,
      'an unverified expired legacy account cannot silently change',
    );
    expired = false;
    await client.getOverview();
    assert.equal((await readSession(legacyFile)).accountId, 'parent-1');
  } finally {
    await client.close();
    routes.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('rejected login, unsafe redirects, challenges, rate limits and malformed authentication fail closed', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-rejected-'));
  const file = join(directory, 'session.json');
  await writeSession(await savedSession(), file);
  const before = await readFile(file, 'utf8');

  try {
    for (const mode of ['rejected', 'redirect', 'challenge', 'rate', 'malformed']) {
      let calls = 0;

      const fetcher = async (): Promise<Response> => {
        calls++;

        if (mode === 'redirect')
          return new Response(null, {
            status: 307,
            headers: { Location: 'https://evil.test/?private=synthetic' },
          });

        if (mode === 'challenge')
          return new Response('<title>Just a moment</title>', { status: 403 });

        if (mode === 'rate')
          return new Response('', { status: 429, headers: { 'Retry-After': '120' } });

        return new Response(mode === 'malformed' ? '{bad-json}' : 'false');
      };

      const http = new InfoMentorHttp(undefined, 0, fetcher);

      if (mode === 'rejected') assert.equal(await http.isAuthenticated(), false);
      else
        await assert.rejects(
          http.isAuthenticated(),
          (error: Error) => !error.message.includes('private=synthetic'),
        );

      if (mode === 'rate') {
        await assert.rejects(http.isAuthenticated(), { code: 'RATE_LIMITED' });
        assert.equal(calls, 1);
      }

      await assert.rejects(importSession(file, { sessionFile: file, fetch: fetcher }));
      assert.equal(await readFile(file, 'utf8'), before);
    }

    const fetcher = async (): Promise<Response> =>
      new Response(
        loginHtml.replace('action="./"', 'action="https://other.infomentor.is/password"'),
      );

    await assert.rejects(
      authenticate(new InfoMentorHttp(undefined, 0, fetcher), { ...credentials }),
      {
        code: 'UNEXPECTED_PAGE',
      },
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('cancelled login/import cannot replace the previous account, even after the last request before the commit', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-commit-'));
  const file = join(directory, 'session.json');
  const transfer = join(directory, 'transfer.json');
  const credentialsFile = join(directory, 'credentials.json');
  const routes = fixture();
  await writeSession(await savedSession(), file);
  await writeSession(await savedSession(), transfer);
  await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });
  const before = await readFile(file, 'utf8');

  try {
    for (const request of [{ credentialsFile }, { importFile: transfer }]) {
      const client = new InfoMentorClient({ sessionFile: file, fetch: routes.fetch });
      const cancelling = Promise.withResolvers<SetupStatus>();
      // Cancelling from the final parent-read callback preserves the existing session; this does
      // not instrument the session-store adapter's own rename check.
      routes.selection.onParent = () => cancelling.resolve(client.cancelSetup());

      try {
        client.startLogin(request);
        assert.equal((await cancelling.promise).state, 'cancelled');
        assert.equal(await readFile(file, 'utf8'), before);
        assert.deepEqual((await readdir(directory)).toSorted(), [
          'credentials.json',
          'session.json',
          'transfer.json',
        ]);
      } finally {
        routes.selection.onParent = () => {};

        await client.close();
      }
    }
  } finally {
    routes.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('login timeout includes session-lock contention and makes no HTTP request', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-login-lock-timeout-'));
  const file = join(directory, 'session.json');
  const credentialsFile = join(directory, 'credentials.json');
  await writeSession(await savedSession(), file);
  await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });
  const entered = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  let requests = 0;

  const holding = withSessionLock(file, undefined, async () => {
    entered.resolve();
    await release.promise;
  });

  try {
    await entered.promise;

    const pending = login({
      sessionFile: file,
      credentialsFile,
      timeoutMs: 1,
      fetch: async () => {
        requests++;

        return new Response('unexpected');
      },
    });

    await assert.rejects(pending, { code: 'LOGIN_TIMEOUT' });
    assert.equal(requests, 0);
    release.resolve();
    await holding;
    assert.deepEqual((await readdir(directory)).toSorted(), ['credentials.json', 'session.json']);
  } finally {
    release.resolve();
    await holding;
    await rm(directory, { recursive: true, force: true });
  }
});

test('a login whose store write committed reports a late removal failure, not a timeout', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-committed-timeout-'));
  const file = join(directory, 'session.json');
  const credentialsFile = join(directory, 'credentials.json');
  const routes = fixture();
  await writeSession(await savedSession(), file);
  await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });
  let verified = false;
  // The login deadline, fired by the test instead of a clock: under load the steps before the
  // store write alone can outlast any short real deadline.
  const deadline = new AbortController();

  // The store write's key read outlasts the login deadline once InfoMentor has verified.
  class SlowKeys extends LocalKeyFileProvider {
    override async getKey(): Promise<Uint8Array> {
      const key = await super.getKey();

      if (verified) deadline.abort(new DOMException('The deadline passed.', 'TimeoutError'));

      return key;
    }
  }

  // After the legacy file was read, a directory in its place makes its removal fail.
  routes.selection.onParent = () => {
    if (verified) return;
    verified = true;
    rmSync(file);
    mkdirSync(file);
  };

  // login's first AbortSignal.timeout is its deadline.
  const timeout = spyOn(AbortSignal, 'timeout').mockImplementationOnce(() => deadline.signal);

  try {
    await assert.rejects(
      login({
        sessionFile: file,
        credentialsFile,
        fetch: routes.fetch,
        timeoutMs: 200,
        keys: new SlowKeys({ path: store.key }),
      }),
      {
        code: 'INVALID_CONFIGURATION',
        message:
          'Cannot remove the plaintext InfoMentor session file. Any encrypted-store change already completed; remove it by hand.',
      },
    );
    // The deadline passed before the record was written.
    assert.equal(deadline.signal.aborted, true);
    assert.deepEqual((await readStored(store)).credentials, credentials);
  } finally {
    timeout.mockRestore();
    routes.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('session and credential files that are world-readable or symlinked are refused for reads and imports', async () => {
  if (process.platform === 'win32') return;
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-guards-'));
  const file = join(directory, 'session.json');
  const destination = join(directory, 'imported.json');
  const credentialsFile = join(directory, 'credentials.json');
  const routes = fixture();

  try {
    await writeSession(await savedSession(), file);
    await chmod(file, 0o644);
    await assert.rejects(readSession(file), unsafe(/chmod 600/));
    const client = new InfoMentorClient({ sessionFile: file, fetch: routes.fetch });

    try {
      await assert.rejects(client.getOverview(), { code: 'INVALID_SESSION' });
    } finally {
      await client.close();
    }

    await assert.rejects(
      importSession(file, { sessionFile: destination, fetch: routes.fetch }),
      unsafe(/chmod 600/),
    );
    await chmod(file, 0o600);
    const symlinkPath = join(directory, 'link.json');
    await symlink(file, symlinkPath);
    await assert.rejects(readSession(symlinkPath), unsafe(/symlink/));
    await assert.rejects(
      importSession(symlinkPath, { sessionFile: destination, fetch: routes.fetch }),
      unsafe(/symlink/),
    );
    await assert.rejects(stat(destination), { code: 'ENOENT' });
    assert.equal(routes.requests.length, 0, 'refused files never reach InfoMentor');
    await importSession(file, { sessionFile: destination, fetch: routes.fetch });
    assert.equal((await readStored(store)).session?.accountId, 'parent-1');

    await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o644 });
    await assert.rejects(readCredentials(credentialsFile), { code: 'INVALID_CONFIGURATION' });
    await chmod(credentialsFile, 0o600);
    await symlink(credentialsFile, join(directory, 'credentials-link.json'));
    await assert.rejects(readCredentials(join(directory, 'credentials-link.json')), {
      code: 'INVALID_CONFIGURATION',
    });
    assert.deepEqual(await readCredentials(credentialsFile), credentials);
  } finally {
    routes.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('explicit login or import cannot silently replace a session verified for another account', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-account-'));
  const file = join(directory, 'session.json');
  const transfer = join(directory, 'transfer.json');
  const credentialsFile = join(directory, 'credentials.json');
  const routes = fixture();
  const otherAccount = await savedSession('other');
  otherAccount.accountId = 'parent-2';
  await writeSession(otherAccount, file);
  await writeSession(await savedSession(), transfer);
  await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });
  const before = await readFile(file, 'utf8');
  const server = createServer({ sessionFile: file, allowSetupTools: true, fetch: routes.fetch });
  const client = new Client({ name: 'account-test', version: '1.0.0' });
  const [serverTransport, clientTransport] = InMemoryTransport.createLinkedPair();

  const finished = async (): Promise<SetupStatus> => {
    let status = setupStatusSchema.parse(
      (await client.callTool({ name: 'infomentor_setup_status', arguments: {} })).structuredContent,
    );

    for (let step = 0; step < 3_000 && status.state === 'running'; step++) {
      await delay(10);
      status = setupStatusSchema.parse(
        (await client.callTool({ name: 'infomentor_setup_status', arguments: {} }))
          .structuredContent,
      );
    }

    return status;
  };

  try {
    await assert.rejects(
      login({ sessionFile: file, credentialsFile, fetch: routes.fetch }),
      mismatch,
    );
    assert.equal(await readFile(file, 'utf8'), before);
    await assert.rejects(
      importSession(transfer, { sessionFile: file, fetch: routes.fetch }),
      mismatch,
    );
    assert.equal(await readFile(file, 'utf8'), before);

    await server.connect(serverTransport);
    await client.connect(clientTransport);
    await client.callTool({ name: 'infomentor_login', arguments: { credentialsFile } });
    const refused = await finished();
    assert.equal(refused.state, 'failed');
    assert.match(refused.message, /different InfoMentor account/);
    assert.equal(await readFile(file, 'utf8'), before);

    await client.callTool({
      name: 'infomentor_login',
      arguments: { credentialsFile, allowAccountChange: true },
    });
    const succeeded = await finished();
    assert.equal(succeeded.state, 'succeeded');
    assert.match(
      succeeded.message,
      new RegExp(`stored in the encrypted store\\. You can delete ${credentialsFile} now\\.$`),
    );
    assert.ok(!succeeded.message.includes(credentials.password));
    await assert.rejects(stat(file), { code: 'ENOENT' });
    assert.equal((await readStored(store)).session?.accountId, 'parent-1');
    assert.deepEqual((await readStored(store)).credentials, credentials);

    // The same account may sign in again.
    await login({ sessionFile: file, credentialsFile, fetch: routes.fetch });
    assert.equal((await readStored(store)).session?.accountId, 'parent-1');

    // A sign-in from the server's configured file never echoes that path to the MCP caller.
    process.env['INFOMENTOR_CREDENTIALS_FILE'] = credentialsFile;

    try {
      await client.callTool({ name: 'infomentor_login', arguments: {} });
      const configured = await finished();
      assert.equal(configured.state, 'succeeded');
      assert.equal(
        configured.message,
        'Session saved in the encrypted store. Call infomentor_session_status to verify access.',
      );
    } finally {
      delete process.env['INFOMENTOR_CREDENTIALS_FILE'];
    }

    // A stored session of another account is protected the same way. An import keeps the stored
    // sign-in only for the same verified account.
    const otherTransfer = join(directory, 'other-transfer.json');
    await writeSession(await savedSession('other'), otherTransfer);
    await importSession(otherTransfer, {
      sessionFile: file,
      fetch: routes.fetch,
      allowAccountChange: true,
    });
    assert.deepEqual(
      [(await readStored(store)).session?.accountId, (await readStored(store)).credentials],
      ['parent-2', null],
    );
    const sealed = await readFile(store.record, 'utf8');
    await assert.rejects(
      login({ sessionFile: file, credentialsFile, fetch: routes.fetch }),
      mismatch,
    );
    await assert.rejects(
      importSession(transfer, { sessionFile: file, fetch: routes.fetch }),
      mismatch,
    );
    assert.equal(await readFile(store.record, 'utf8'), sealed);
    await login({
      sessionFile: file,
      credentialsFile,
      fetch: routes.fetch,
      allowAccountChange: true,
    });
    await importSession(transfer, { sessionFile: file, fetch: routes.fetch });
    assert.deepEqual((await readStored(store)).credentials, credentials);

    // Before any store exists, plaintext files without a verified account are replaceable.
    await anotherHome(store);
    await writeSession(await savedSession('other'), file);
    await login({ sessionFile: file, credentialsFile, fetch: routes.fetch });
    assert.equal((await readStored(store)).session?.accountId, 'parent-1');
    await anotherHome(store);
    await writeFile(file, '{"version":1}', { mode: 0o600 });
    await importSession(transfer, { sessionFile: file, fetch: routes.fetch });
    assert.equal((await readStored(store)).session?.accountId, 'parent-1');
  } finally {
    await client.close();
    await server.close();
    routes.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('explicit login refuses unreadable saved sessions unless account change is allowed', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-unreadable-account-'));
  const file = join(directory, 'session.json');
  const credentialsFile = join(directory, 'credentials.json');
  const alias = join(directory, 'alias.json');
  const routes = fixture();
  await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });

  const replace = (allowAccountChange = false) =>
    login({ sessionFile: file, credentialsFile, fetch: routes.fetch, allowAccountChange });

  const cannotVerify = {
    code: 'INVALID_CONFIGURATION',
    message: /cannot be verified/,
  };

  try {
    await writeSession(await savedSession('other'), file);

    if (process.platform !== 'win32') {
      await chmod(file, 0o644);
      const before = await readFile(file, 'utf8');
      await assert.rejects(replace(), cannotVerify);
      assert.equal(await readFile(file, 'utf8'), before);
      await replace(true);
      assert.equal((await readStored(store)).session?.accountId, 'parent-1');
    }

    // Each case starts before any store exists, while the plaintext file still decides.
    await anotherHome(store);
    await writeFile(file, 'x'.repeat(SESSION_MAX_BYTES + 1), { mode: 0o600 });
    const oversized = await readFile(file, 'utf8');
    await assert.rejects(replace(), cannotVerify);
    assert.equal(await readFile(file, 'utf8'), oversized);
    await replace(true);
    assert.equal((await readStored(store)).session?.accountId, 'parent-1');

    if (process.platform !== 'win32') {
      await anotherHome(store);
      await writeSession(await savedSession('other'), file);
      await link(file, alias);
      const before = await readFile(file, 'utf8');
      await assert.rejects(replace());
      // The shared lock rejects hard-linked targets even when account changes are allowed.
      await assert.rejects(replace(true));
      assert.equal(await readFile(file, 'utf8'), before);
      await rm(alias);
    }
  } finally {
    routes.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('a rate-limit pause is saved with the session and honoured by other processes without contacting InfoMentor', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-rate-'));
  const file = join(directory, 'session.json');
  const otherFile = join(directory, 'other.json');
  await writeSession(await savedSession(), file);
  await writeSession(await savedSession(), otherFile);
  let calls = 0;

  const fetcher = async (): Promise<Response> => {
    calls++;

    return new Response('', { status: 429, headers: { 'Retry-After': '120' } });
  };

  const first = new InfoMentorClient({ sessionFile: file, fetch: fetcher });

  try {
    await assert.rejects(first.getOverview(), limited(100_000, 120_000));
    assert.equal(calls, 1);
    const saved = await readSession(file);
    assert.ok(saved.rateLimitedUntil);
    const until = Date.parse(saved.rateLimitedUntil);
    assert.ok(until > Date.now() + 100_000 && until <= Date.now() + 120_000);

    const second = new InfoMentorClient({ sessionFile: file, fetch: fetcher });

    try {
      await assert.rejects(second.getOverview(), limited(100_000, 120_000));
      assert.equal(calls, 1, 'a second process waits without a request');
      assert.equal((await readSession(file)).rateLimitedUntil, saved.rateLimitedUntil);
    } finally {
      await second.close();
    }

    const third = new InfoMentorClient({ sessionFile: otherFile, fetch: fetcher });

    try {
      await assert.rejects(third.getOverview(), { code: 'RATE_LIMITED' });
      assert.equal(calls, 2, 'another session file is not affected');
    } finally {
      await third.close();
    }

    const expired = await savedSession();
    expired.rateLimitedUntil = new Date(Date.now() - 1000).toISOString();
    await writeSession(expired, otherFile);
    const absurd = await savedSession();
    absurd.rateLimitedUntil = new Date(Date.now() + 10 * 24 * 60 * 60 * 1000).toISOString();
    await writeSession(absurd, file);
    const fourth = new InfoMentorClient({ sessionFile: otherFile, fetch: fetcher });
    const fifth = new InfoMentorClient({ sessionFile: file, fetch: fetcher });

    try {
      await assert.rejects(fourth.getOverview(), { code: 'RATE_LIMITED' });
      assert.equal(calls, 3, 'an expired pause is not honoured');
      await assert.rejects(fifth.getOverview(), limited(0, 3_600_000));
      assert.equal(calls, 3, 'a saved pause is capped at one hour');
    } finally {
      await fourth.close();
      await fifth.close();
    }
  } finally {
    await first.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('an oversized Retry-After is capped with rotated cookies and honoured by another client', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-rate-cap-'));
  const file = join(directory, 'session.json');
  const actualNow = Date.now;
  let now = actualNow();
  await writeSession(await savedSession(), file);
  let calls = 0;

  const fetcher = async (input: string | URL | Request): Promise<Response> => {
    calls++;

    if (calls === 1)
      return new Response('', {
        status: 429,
        headers: {
          'Retry-After': '9999999999999',
          'Set-Cookie': 'IMHome=rotated; Secure; HttpOnly; Path=/',
        },
      });

    const url = input instanceof Request ? new URL(input.url) : new URL(input.toString());

    if (url.href === PARENT_URL)
      return new Response(
        `<script>IMHome.home.homeData = ${JSON.stringify(parent)}; IMHome.home.init(IMHome.home.homeData);</script>`,
      );

    return Response.json({ items: [] });
  };

  Date.now = () => now;
  const first = new InfoMentorClient({ sessionFile: file, fetch: fetcher });

  try {
    await assert.rejects(first.getOverview(), { code: 'RATE_LIMITED' });
    assert.equal(calls, 1);
    const saved = await readSession(file);
    assert.ok(saved.rateLimitedUntil);
    const until = Date.parse(saved.rateLimitedUntil);
    assert.ok(until > Date.now() + MAX_RATE_LIMIT_MS - 10_000);
    assert.ok(until <= Date.now() + MAX_RATE_LIMIT_MS);
    assert.ok(
      saved.cookies.some((cookie) => cookie.key === 'IMHome' && cookie.value === 'rotated'),
    );

    const second = new InfoMentorClient({ sessionFile: file, fetch: fetcher });

    try {
      await assert.rejects(
        second.getOverview(),
        limited(MAX_RATE_LIMIT_MS - 10_000, MAX_RATE_LIMIT_MS),
      );
      assert.equal(calls, 1, 'the saved cooldown prevents a second upstream request');
    } finally {
      await second.close();
    }

    now = until + 1;
    await first.getOverview();
    assert.equal(
      calls,
      3,
      'the originating client makes requests after the fixed cooldown expires',
    );
    assert.equal(
      (await readSession(file)).rateLimitedUntil,
      undefined,
      'an expired cooldown is cleared instead of being re-armed from the retry time',
    );
  } finally {
    Date.now = actualNow;
    await first.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('HTTP cancellation aborts in-flight requests; closing a client drains reads', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'infomentor-cancel-'));
  const file = join(directory, 'session.json');
  await writeSession(await savedSession(), file);
  let active = 0;
  const entered = Promise.withResolvers<void>();

  const fetcher = async (_input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    active++;
    entered.resolve();

    try {
      await delay(60_000, undefined, { signal: init?.signal ?? undefined });

      return new Response('true');
    } finally {
      active--;
    }
  };

  try {
    const client = new InfoMentorClient({ sessionFile: file, fetch: fetcher });

    try {
      const reading = assert.rejects(client.getOverview(), { code: 'CANCELLED' });
      await entered.promise;
      await client.close();
      await reading;
      assert.equal(active, 0);
    } finally {
      await client.close();
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

/** The fixture, with an expiry InfoMentor ends when a password is submitted. */
function expiring() {
  const routes = fixture();
  const state = { expired: false, submissions: 0 };

  const fetcher = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    assert.ok(input instanceof URL);

    if ((await new Response(init?.body ?? null).text()).includes('txtLykilord')) {
      state.submissions++;
      state.expired = false;
    }

    if (state.expired && input.href === PARENT_URL) return new Response('', { status: 401 });

    return routes.fetch(input, init);
  };

  return { routes, state, fetch: fetcher };
}

/** A scratch tree with the older plaintext session's directory apart from the user's inputs. */
async function scratchTree(prefix: string) {
  const directory = await mkdtemp(join(tmpdir(), prefix));
  const state = join(directory, 'state');
  const input = join(directory, 'input');
  await mkdir(state, { mode: 0o700 });
  await mkdir(input, { mode: 0o700 });
  const credentialsFile = join(input, 'credentials.json');
  await writeFile(credentialsFile, JSON.stringify(credentials), { mode: 0o600 });

  return { directory, state, input, file: join(state, 'session.json'), credentialsFile };
}

test('login, import, renewal and migrate leave no session cookie, username or password in plaintext', async () => {
  const tree = await scratchTree('infomentor-plaintext-');
  const { file, credentialsFile } = tree;
  const { state, fetch } = expiring();
  const transfer = join(tree.input, 'transfer.json');
  await writeSession(await savedSession(), transfer);

  // The session JSON names its cookie; the key, marker and ciphertext never contain these.
  const plaintext = () =>
    filesContaining(['IMHome', credentials.username, credentials.password], tree.state, store.home);

  try {
    await writeSession(await savedSession(), file);
    assert.equal(await migrate(file, credentialsFile), 'migrated');
    assert.deepEqual(await plaintext(), []);
    assert.deepEqual((await readStored(store)).credentials, credentials);

    await importSession(transfer, { sessionFile: file, fetch });
    assert.deepEqual(await plaintext(), []);

    await login({ sessionFile: file, credentialsFile, fetch });
    assert.deepEqual(await plaintext(), []);

    const client = new InfoMentorClient({ sessionFile: file, fetch });

    try {
      state.expired = true;
      await client.getOverview();
      assert.equal(state.submissions, 2);
      assert.deepEqual(await plaintext(), []);
    } finally {
      await client.close();
    }
  } finally {
    await rm(tree.directory, { recursive: true, force: true });
  }
});

test('auth migrate moves the plaintext session once, optionally with the sign-in, and never reads it again', async () => {
  const tree = await scratchTree('infomentor-migrate-');
  const { file, credentialsFile } = tree;
  const routes = fixture();
  const verified = await savedSession();
  verified.accountId = 'parent-1';
  const cursor = join(`${file}.collections`, 'cursor.json');

  const status = async () => {
    const client = new InfoMentorClient({ sessionFile: file, fetch: routes.fetch });

    try {
      return await client.getSessionStatus();
    } finally {
      await client.close();
    }
  };

  try {
    // Before migration the plaintext file decides, as it always did.
    await writeSession(verified, file);
    await mkdir(`${file}.collections`, { mode: 0o700 });
    await writeFile(cursor, '{"synthetic":true}', { mode: 0o600 });
    assert.equal(
      (await status()).storage,
      'Saved in a plaintext file. Run infomentor-mcp auth migrate.',
    );

    // The status read saved the cookies it rotated in the plaintext file.
    const legacy = await readSession(file);
    assert.equal(await migrate(file), 'migrated');
    assert.deepEqual(await readStored(store), { version: 1, session: legacy, credentials: null });
    await assert.rejects(stat(file), { code: 'ENOENT' });
    assert.equal(await readFile(cursor, 'utf8'), '{"synthetic":true}', 'cursors stay in place');
    assert.equal(await migrate(file), 'already');

    // A plaintext file planted after the marker is removed unread and never used.
    const sealed = await readFile(store.record, 'utf8');
    await writeSession(await savedSession('other'), file);
    assert.equal(await migrate(file), 'already-removed-legacy');
    await assert.rejects(stat(file), { code: 'ENOENT' });
    assert.equal(await readFile(store.record, 'utf8'), sealed);
    await writeSession(await savedSession('other'), file);
    const current = await status();
    assert.equal(current.authenticated, true);
    assert.equal(
      current.storage,
      'Saved in an encrypted file. No InfoMentor sign-in is stored for automatic renewal.',
    );
    assert.ok(!routes.requests.some(({ cookies }) => cookies.includes('IMHome=other')));

    // Logout keeps the store deciding: a planted file is never read again.
    await logout(file);
    await assert.rejects(stat(`${file}.collections`), { code: 'ENOENT' });
    await writeSession(await savedSession('other'), file);
    assert.equal((await status()).authenticated, false);
    assert.equal(await migrate(file), 'already-removed-legacy');

    // A store with a marker but no record (a reset, or a first write cut short) refuses reads
    // and resumes an explicit migration.
    await anotherHome(store);
    await writeSession(verified, file);
    assert.equal(await migrate(file), 'migrated');
    await rm(store.key);
    await resetSecretStore(recordOptions(store));
    await writeSession(verified, file);
    // Only an explicit login or import may replace a lost key; migrate never resets the store.
    await assert.rejects(migrate(file), /store key is missing/);
    await assert.rejects(stat(store.key), { code: 'ENOENT' });
    await createSecretKey(recordOptions(store));
    await writeSession(verified, file);
    assert.equal((await status()).authenticated, false);
    assert.equal(await migrate(file, credentialsFile), 'migrated');
    assert.deepEqual(await readStored(store), { version: 1, session: verified, credentials });

    // The CLI stores the sign-in and names the file the user typed.
    await anotherHome(store);
    await writeSession(verified, file);
    const preload = join(tree.input, 'no-network.js');
    await writeFile(
      preload,
      `globalThis.fetch = () => process.exit(92);\nrequire('node:http').Server.prototype.listen = () => process.exit(91);\n`,
    );

    const run = async (...args: string[]) => {
      const child = Bun.spawn(
        [process.execPath, '--preload', preload, join(import.meta.dir, '../src/cli.ts'), ...args],
        {
          cwd: tree.input,
          env: {
            HOME: store.home,
            XDG_CONFIG_HOME: process.env['XDG_CONFIG_HOME'],
            XDG_DATA_HOME: process.env['XDG_DATA_HOME'],
            FAMILY_MCP_STORE_TEST_SEAM: '1',
            INFOMENTOR_SESSION_PATH: file,
          },
          timeout: 10_000,
          stdout: 'pipe',
          stderr: 'pipe',
        },
      );

      return { code: await child.exited, stderr: await new Response(child.stderr).text() };
    };

    const migrated = await run('auth', 'migrate', '--credentials', 'credentials.json');
    assert.equal(migrated.code, 0);
    assert.match(
      migrated.stderr,
      /Your InfoMentor sign-in is stored in the encrypted store\. You can delete credentials\.json now\./,
    );
    assert.deepEqual((await readStored(store)).credentials, credentials);
    const again = await run('auth', 'migrate', '--credentials', 'credentials.json');
    assert.equal(again.code, 0);
    assert.match(again.stderr, /already in the encrypted store[\s\S]*was not stored/);
    assert.equal((await run('auth')).code, 1);
  } finally {
    await rm(tree.directory, { recursive: true, force: true });
  }
});

test('session paths that overlap the store or its key are refused before anything is touched', async () => {
  const tree = await scratchTree('infomentor-collision-');
  const { credentialsFile } = tree;
  const routes = fixture();
  await login({ sessionFile: tree.file, credentialsFile, fetch: routes.fetch });
  const record = await readFile(store.record);
  const key = await readFile(store.key);
  const requests = routes.requests.length;
  const alias = join(tree.directory, 'alias');
  await symlink(dirname(store.record), alias);

  const operations = (sessionFile: string) => [
    () => login({ sessionFile, credentialsFile, fetch: routes.fetch, allowAccountChange: true }),
    () =>
      importSession(tree.credentialsFile, {
        sessionFile,
        fetch: routes.fetch,
        allowAccountChange: true,
      }),
    () => migrate(sessionFile, credentialsFile),
    () => logout(sessionFile),
    async () => {
      const client = new InfoMentorClient({ sessionFile, fetch: routes.fetch });

      try {
        await client.getOverview();
      } finally {
        await client.close();
      }
    },
  ];

  try {
    for (const sessionFile of [
      store.record,
      `${store.record}.marker`,
      `${store.record}.lock`,
      `${store.record}.0b6f6a4e-5d2c-4a5e-9a59-3c2f1f0d9e71.tmp`,
      join(`${store.record}.lock`, 'session.json'),
      dirname(store.record),
      join(dirname(store.record), 'session'),
      join(alias, 'session.enc'),
      store.key,
      dirname(store.key),
      join(dirname(store.key), 'infomentor-mcp'),
      join(dirname(dirname(store.record)), 'infomentor-mcp'),
    ])
      for (const operation of operations(sessionFile))
        await assert.rejects(operation(), (error: Error) => {
          assert.ok(error instanceof InfoMentorError);
          assert.match(error.message, /overlaps the encrypted InfoMentor session store/);
          assert.ok(!error.message.includes(tree.directory) && !error.message.includes(store.home));

          return true;
        });

    assert.deepEqual(await readFile(store.record), record);
    assert.deepEqual(await readFile(store.key), key);
    assert.equal(routes.requests.length, requests);

    // The collection cursors beside the plaintext file must stay outside the store too.
    await anotherHome(store);
    const collections = join(tree.directory, 'cursors.json.collections');
    process.env['XDG_CONFIG_HOME'] = collections;
    await assert.rejects(
      migrate(join(tree.directory, 'cursors.json')),
      /overlaps the encrypted InfoMentor session store/,
    );
    await assert.rejects(stat(collections), { code: 'ENOENT' });
  } finally {
    await rm(tree.directory, { recursive: true, force: true });
  }
});

test('store holders serialize: reads wait for a held store, and two expired clients renew once', async () => {
  const tree = await scratchTree('infomentor-serial-');
  const { file, credentialsFile } = tree;
  const { routes, state, fetch } = expiring();
  await login({ sessionFile: file, credentialsFile, fetch });
  await rm(credentialsFile);
  const first = new InfoMentorClient({ sessionFile: file, fetch });
  const second = new InfoMentorClient({ sessionFile: file, fetch });

  try {
    const entered = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();

    const holding = withSecretStore(recordOptions(store), async () => {
      entered.resolve();
      await release.promise;
    });

    await entered.promise;
    const before = routes.requests.length;
    let done = false;

    const reading = first.getOverview().then(() => {
      done = true;

      return done;
    });

    await delay(400);
    assert.equal(done, false);
    assert.equal(
      routes.requests.length,
      before,
      'no request while another process holds the store',
    );
    release.resolve();
    await holding;
    await reading;

    // Without the store lock both clients would see the expiry and submit the password.
    state.expired = true;
    const submissions = state.submissions;
    await Promise.all([first.getOverview(), second.getOverview()]);
    assert.equal(state.submissions, submissions + 1);

    // Admin commands also wait for the plaintext file's lock.
    const locked = Promise.withResolvers<void>();
    const unlock = Promise.withResolvers<void>();

    const legacyHold = withSessionLock(file, undefined, async () => {
      locked.resolve();
      await unlock.promise;
    });

    await locked.promise;
    let migrated = false;

    const migrating = migrate(file).then((result) => {
      migrated = true;

      return result;
    });

    await delay(400);
    assert.equal(migrated, false);
    unlock.resolve();
    await legacyHold;
    assert.equal(await migrating, 'already');
  } finally {
    await first.close();
    await second.close();
    await rm(tree.directory, { recursive: true, force: true });
  }
});

test('store failures give fixed messages without paths and never fall back to the plaintext file', async () => {
  const tree = await scratchTree('infomentor-store-errors-');
  const { file, credentialsFile } = tree;
  const routes = fixture();
  const planted = JSON.stringify(await savedSession('other'));

  const refused = (pattern: RegExp) => (error: Error) => {
    assert.ok(error instanceof InfoMentorError, String(error));
    assert.match(error.message, pattern);

    for (const secret of [tree.directory, store.home, credentials.password, 'IMHome'])
      assert.ok(!error.message.includes(secret));

    return true;
  };

  const read = async (keys?: KeyProvider) => {
    const client = new InfoMentorClient({
      sessionFile: file,
      fetch: routes.fetch,
      ...(keys && { keys }),
    });

    try {
      return await client.getSessionStatus();
    } finally {
      await client.close();
    }
  };

  // Every store-side path refuses, the plaintext file stays unread, and nothing is requested.
  const allRefuse = async (pattern: RegExp) => {
    const requests = routes.requests.length;
    await writeFile(file, planted, { mode: 0o600 });
    await assert.rejects(read(), refused(pattern));
    await assert.rejects(migrate(file), refused(pattern));
    await assert.rejects(logout(file), refused(pattern));
    assert.equal(await readFile(file, 'utf8'), planted);
    assert.equal(routes.requests.length, requests);
  };

  try {
    await login({ sessionFile: file, credentialsFile, fetch: routes.fetch });

    // A lost key: reads, migrate and logout refuse; only an explicit login resets the store.
    const key = await readFile(store.key);
    await rm(store.key);
    await allRefuse(/store key is missing\. Run infomentor-mcp login/);
    await login({ sessionFile: file, credentialsFile, fetch: routes.fetch });
    await assert.rejects(stat(file), { code: 'ENOENT' });
    assert.notDeepEqual(await readFile(store.key), key);
    assert.equal((await read()).authenticated, true);

    // A record lost after its marker committed: even login refuses before submitting the password.
    await rm(store.record);
    await allRefuse(/did not complete, so its session is not used/);
    const requests = routes.requests.length;
    await assert.rejects(
      login({ sessionFile: file, credentialsFile, fetch: routes.fetch, allowAccountChange: true }),
      refused(/did not complete/),
    );
    assert.equal(routes.requests.length, requests);

    // A tampered record fails authentication.
    await anotherHome(store);
    await login({ sessionFile: file, credentialsFile, fetch: routes.fetch });
    const [header, body] = (await readFile(store.record, 'utf8')).split('\n');
    assert.ok(header && body);
    await writeFile(
      store.record,
      `${header}\n${body.startsWith('A') ? 'B' : 'A'}${body.slice(1)}\n`,
      { mode: 0o600 },
    );
    await allRefuse(/damaged, unsafe, or not readable/);

    // A key that fails refuses, and the key is never created as a fallback.
    await anotherHome(store);
    await login({ sessionFile: file, credentialsFile, fetch: routes.fetch });

    const refusing: KeyProvider = {
      backend: 'unsafe',
      keySource: 'unsafe-key',
      keyId: 'test',
      getKey: async () => {
        throw new SessionStoreError('UNSAFE_FILE', 'Unsafe.');
      },
      createKey: async () => assert.fail('An unsafe key must not get a new key.'),
    };

    await assert.rejects(
      read(refusing),
      refused(
        /Run infomentor-mcp status in a terminal; it shows what is wrong and where\. Do not delete the store first\./,
      ),
    );
    await assert.rejects(
      login({ sessionFile: file, credentialsFile, fetch: routes.fetch, keys: refusing }),
      refused(
        /Run infomentor-mcp status in a terminal; it shows what is wrong and where\. Do not delete the store first\./,
      ),
    );
  } finally {
    await rm(tree.directory, { recursive: true, force: true });
  }
});

test('the record bound covers the largest storable session and sign-in', async () => {
  const tree = await scratchTree('infomentor-size-');
  const session = await savedSession();
  session.accountId = 'parent-1';
  const [cookie] = session.cookies;
  assert.ok(cookie);
  cookie.value = '';
  cookie.value = 'x'.repeat(SESSION_MAX_BYTES - Buffer.byteLength(JSON.stringify(session)));
  assert.equal(Buffer.byteLength(JSON.stringify(session)), SESSION_MAX_BYTES);

  const worst = {
    version: 1 as const,
    session,
    credentials: {
      username: '\u0001'.repeat(USERNAME_MAX),
      password: '\u0001'.repeat(PASSWORD_MAX),
    },
  };

  try {
    assert.equal(Buffer.byteLength(encodeRecord(worst)), RECORD_MAX_BYTES);
    cookie.value += 'x';
    assert.throws(() => encodeRecord(worst), /larger than the store allows/);
    cookie.value = cookie.value.slice(1);

    // The largest session an older version could save migrates into the store unchanged.
    await writeSession(session, tree.file);
    assert.equal(await migrate(tree.file, tree.credentialsFile), 'migrated');
    assert.deepEqual(await readStored(store), { version: 1, session, credentials });
  } finally {
    await rm(tree.directory, { recursive: true, force: true });
  }
});
