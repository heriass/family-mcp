import { expect, test } from 'bun:test';
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { COOKIES, fresh, startFake, type Seen } from './fake-inna.ts';

const binary = process.env.INNA_RUST_BINARY!;
const USER = '/api/UserData/GetLoggedInUser';
const SCHOOL = '/api/Announcements/GetStudentAnnouncements';
const POST = '/api/RegisterAbsence/AddNewLeave';
const SWITCH = '/auth/system';
const WARNING = 'Inna renewal failed. School requests require successful renewal.';

async function until(check: () => boolean, timeout = 2_000) {
  const end = performance.now() + timeout;
  while (!check()) {
    if (performance.now() >= end) throw new Error('The fake request did not arrive in time.');
    await Bun.sleep(5);
  }
}

async function fixture(bound = 1_500, args: string[] = []) {
  const directory = mkdtempSync(join(process.env.TMPDIR!, 'renewal-'));
  const state = fresh();
  const seen: Seen[] = [];
  const fake = await startFake(() => ({ state, seen }));
  const env = {
    ...process.env,
    HOME: directory,
    XDG_CONFIG_HOME: join(directory, 'config'),
    XDG_DATA_HOME: join(directory, 'data'),
    INNA_SESSION_FILE: join(directory, 'legacy.json'),
    FAMILY_MCP_STORE_TEST_SEAM: '1',
    INNA_TEST_ORIGIN: fake.origin,
    INNA_TEST_RENEWAL_MS: String(bound),
    INNA_TEST_KEEP_ALIVE: '1',
  };
  mkdirSync(env.XDG_CONFIG_HOME, { mode: 0o700 });
  const source = join(directory, 'cookies.json');
  writeFileSync(source, COOKIES, { mode: 0o600 });
  chmodSync(source, 0o600);
  const cli = async (args: string[]) => {
    const child = Bun.spawn([binary, ...args], {
      env,
      stdin: 'ignore',
      stdout: 'pipe',
      stderr: 'pipe',
    });
    const [code, stdout, stderr] = await Promise.all([
      child.exited,
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
    ]);
    expect(code).toBe(0);
    expect(stdout + stderr).not.toContain('synthetic-');
  };
  await cli(['auth', 'import', source]);
  const started = performance.now();
  const child = Bun.spawn([binary, 'serve', ...args], {
    env,
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const pending = new Map<number, (value: any) => void>();
  let id = 0;
  let stdout = '';
  let errors = '';
  // Set by `deafen`: the next message read is the last, then the read side of stdout closes.
  let deaf = false;
  const stderr = (async () => {
    for await (const chunk of child.stderr) errors += new TextDecoder().decode(chunk);
    return errors;
  })();
  const output = (async () => {
    let buffer = '';
    for await (const chunk of child.stdout) {
      const text = new TextDecoder().decode(chunk);
      stdout += text;
      buffer += text;
      let end: number;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const message = JSON.parse(buffer.slice(0, end));
        buffer = buffer.slice(end + 1);
        pending.get(message.id)?.(message.result);
        pending.delete(message.id);
      }
      if (deaf) break;
    }
  })();
  const request = (method: string, params: object) => {
    const number = ++id;
    const answer = new Promise<any>((resolve) => pending.set(number, resolve));
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: number, method, params })}\n`);
    return answer;
  };
  await request('initialize', {
    protocolVersion: '2025-06-18',
    capabilities: {},
    clientInfo: { name: 'renewal-test', version: '1' },
  });
  child.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');
  const call = (name = 'inna_get_overview', args: object = {}) =>
    request('tools/call', { name, arguments: args });
  const paths = (from = 0) => seen.slice(from).map((request) => new URL(request.url).pathname);
  const close = async () => {
    child.stdin.end();
    await child.exited;
    await output;
    const errors = await stderr;
    const text = stdout + errors;
    // Every value, including rotations and the second process's import, uses this prefix.
    expect(text).not.toContain('synthetic-');
    expect(text).not.toContain('DO-NOT-RETURN');
    await fake.close();
    rmSync(directory, { recursive: true, force: true });
    return { stdout, stderr: errors };
  };
  // The startup tick must finish saving its rotation before a case arms faults or counts calls.
  try {
    await until(() => errors.includes('keep-alive: kept\n'), bound / 30 + 2_000);
  } catch (error) {
    await close();
    throw error;
  }
  /** The host closes its read side of stdout after one more answer, as a broken pipe. */
  const deafen = async () => {
    deaf = true;
    await request('ping', {});
    await output;
  };
  const tick = () => process.kill(child.pid, 'SIGUSR1');
  return {
    state,
    seen,
    paths,
    call,
    close,
    cli,
    source,
    started,
    deafen,
    tick,
    stdin: child.stdin,
    exited: child.exited,
    kill: () => child.kill('SIGKILL'),
  };
}

test('idle serve survives several TTLs within the scaled 30-minute bound, including the flag', async () => {
  for (const args of [[], ['--no-keep-alive']]) {
    const f = await fixture(1_500, args);
    try {
      f.state.ttl = 3_000;
      f.state.renewalGaps = [];
      await Bun.sleep(9_300);
      expect(f.state.rotations).toBeGreaterThan(6);
      expect(Math.max(...f.state.renewalGaps)).toBeLessThan(1_500);
      expect(f.paths().every((path) => path === USER)).toBe(true);
      expect((await f.call()).isError).not.toBe(true);
    } finally {
      await f.close();
    }
  }
});

test('401 and 5xx renewals fail closed past the bound without a school request', async () => {
  for (const status of [401, 500]) {
    const f = await fixture(300);
    try {
      expect((await f.call()).isError).not.toBe(true);
      f.state.planted[USER] = { status };
      await Bun.sleep(450);
      const from = f.seen.length;
      const result = await f.call();
      expect(result.isError).toBe(true);
      expect(JSON.stringify(result)).toContain(
        status === 401 ? 'sign-in is required' : 'unavailable or unexpected',
      );
      expect(f.paths(from).every((path) => path === USER)).toBe(true);
      delete f.state.planted[USER];
      expect((await f.call()).isError).not.toBe(true);
    } finally {
      await f.close();
    }
  }
});

test('read 401 refreshes once and retries once; a second 401 has no third school request', async () => {
  for (const [failures, studentKey] of [
    [1, undefined],
    [2, undefined],
    [1, '5'],
  ] as const) {
    const f = await fixture(60_000);
    try {
      f.state.once401 = { [SCHOOL]: failures };
      const from = f.seen.length;
      const result = await f.call('inna_get_overview', { studentKey });
      expect(Boolean(result.isError)).toBe(failures === 2);
      const paths = f.paths(from);
      expect(paths.filter((path) => path === SCHOOL)).toHaveLength(2);
      const first = paths.indexOf(SCHOOL);
      expect(paths.slice(first, first + 3)).toEqual([SCHOOL, USER, SCHOOL]);
      if (failures === 2) expect(JSON.stringify(result)).toContain('sign-in is required');
    } finally {
      await f.close();
    }
  }
});

test('a separate CLI import is used by the next call', async () => {
  const f = await fixture(60_000);
  try {
    await f.call();
    await f.cli(['auth', 'import', f.source]);
    const rotated = `synthetic-rotated-${f.state.rotations}`;
    f.state.once401 = { [USER]: 1 };
    const from = f.seen.length;
    expect((await f.call()).isError).not.toBe(true);
    expect(f.seen[from]?.headers.cookie).toContain(rotated);
    expect(f.paths(from).slice(0, 3)).toEqual([USER, USER, USER]);
  } finally {
    await f.close();
  }
});

test('renewal age is checked again between school requests within one call', async () => {
  const f = await fixture(300);
  try {
    const first = '/api/Announcements/GetStudentAnnouncements';
    f.state.delays[first] = 350;
    f.state.afterUser = () => {
      f.state.planted[USER] = { status: 500 };
    };
    const from = f.seen.length;
    const result = await f.call();
    expect(result.isError).toBe(true);
    expect(JSON.stringify(result)).toContain('unavailable or unexpected');
    expect(f.paths(from).slice(0, 3)).toEqual([USER, first, USER]);
    expect(f.paths(from).filter((path) => path !== USER)).toEqual([first]);
  } finally {
    await f.close();
  }
});

test('401 on absence POST sends once and retains the uncertain outcome', async () => {
  const f = await fixture(60_000, ['--allow-absence-writes']);
  try {
    f.state.planted['/api/RegisterAbsence/GetStudentRegisteredAbsences'] = {
      status: 200,
      body: '[]',
    };
    f.state.planted['/api/RegisterAbsence/GetStudentLeaves'] = { status: 200, body: '[]' };
    const day = new Date().toISOString().slice(0, 10);
    const preview = await f.call('inna_prepare_absence', {
      kind: 'leave',
      dateFrom: day,
      dateTo: day,
      reason: 'Test',
    });
    expect(preview.isError).not.toBe(true);
    f.state.planted[POST] = { status: 401 };
    const result = await f.call('inna_submit_absence', {
      operationId: preview.structuredContent.operationId,
      confirm: true,
    });
    expect(result.isError).toBe(true);
    expect(JSON.stringify(result)).toContain('outcome is uncertain');
    expect(f.paths().filter((path) => path === POST)).toHaveLength(1);
    const status = await f.call('inna_absence_status');
    expect(status.structuredContent.operation.state).toBe('unknown');
  } finally {
    await f.close();
  }
});

test('serve start renews within bound/30 plus slack in both modes', async () => {
  const bound = 6_000;
  const slack = 400;
  for (const args of [[], ['--no-keep-alive']]) {
    const f = await fixture(bound, args);
    try {
      await until(() => f.state.rotations >= 2, bound / 30 + slack);
      expect(performance.now() - f.started).toBeLessThan(bound / 30 + slack);
      expect(f.paths().every((path) => path === USER)).toBe(true);
    } finally {
      await f.close();
    }
  }
});

test('a switch 401 recovers once and a persistent 401 refuses after exactly two switches', async () => {
  for (const persistent of [false, true]) {
    const f = await fixture(60_000);
    try {
      if (persistent) f.state.planted[SWITCH] = { status: 401 };
      else f.state.once401 = { [SWITCH]: 1 };
      const from = f.seen.length;
      const result = await f.call('inna_get_overview', { studentKey: '5' });
      expect(Boolean(result.isError)).toBe(persistent);
      const paths = f.paths(from);
      expect(paths.filter((path) => path === SWITCH)).toHaveLength(2);
      const first = paths.indexOf(SWITCH);
      expect(paths.slice(first, first + 3)).toEqual([SWITCH, USER, SWITCH]);
      if (persistent)
        expect(JSON.stringify(result)).toContain(
          'Inna refused the student switch and asked for sign-in.',
        );
    } finally {
      await f.close();
    }
  }
});

test('a 500 renewal after a checked read gets 401 keeps the transient text and sends no retry', async () => {
  const f = await fixture(60_000);
  try {
    f.state.afterUser = () => {
      f.state.planted[USER] = { status: 500 };
    };
    f.state.once401 = { [SCHOOL]: 1 };
    const from = f.seen.length;
    const result = await f.call();
    expect(result.isError).toBe(true);
    expect(JSON.stringify(result)).toContain('unavailable or unexpected');
    expect(f.paths(from)).toEqual([USER, SCHOOL, USER]);
  } finally {
    await f.close();
  }
});

test('failed ticks retry before cadence and warn exactly once past the age bound', async () => {
  const bound = 1_500;
  const retry = bound / 30;
  const cadence = (bound * 2) / 3;
  const f = await fixture(bound, ['--no-keep-alive']);
  let output: Awaited<ReturnType<typeof f.close>>;
  try {
    await until(() => f.state.rotations >= 2);
    const keptAt = f.state.renewedAt!;
    f.state.planted[USER] = { status: 500 };
    const from = f.seen.length;
    await until(() => f.seen.length > from);
    const failedAt = performance.now();
    const failed = f.seen.length;
    await until(() => f.seen.length >= failed + 2, cadence / 2);
    expect(performance.now() - failedAt).toBeLessThan(cadence / 2);
    await until(() => Date.now() - keptAt > bound + retry * 3);
    delete f.state.planted[USER];
    await until(() => f.state.rotations >= 3, cadence / 2);
    expect(f.paths().every((path) => path === USER)).toBe(true);
  } finally {
    output = await f.close();
  }
  expect(output.stderr.split(WARNING).length - 1).toBe(1);
});

// Rust: the binary's counterpart of integration.test.ts:2113 and :2132, which run the TypeScript
// scheduler under the SDK runtime in-process. A broken connection while the host still holds
// stdin open (a failed stdout write, or one input line over the SDK's 10 MiB read buffer) ends
// serve with code 0 well inside the stalled tick's delay: closing stops the keep-alive and aborts
// its tick in flight, without which the close would wait for that tick.
test('a broken output pipe or an oversized input line ends serve and aborts the tick in flight', async () => {
  for (const breaks of ['output', 'input'] as const) {
    const f = await fixture(60_000);
    try {
      f.state.delays[USER] = 60_000;
      const from = f.seen.length;
      f.tick();
      await until(() => f.paths(from).includes(USER));

      if (breaks === 'output') {
        await f.deafen();
        f.stdin.write('{"jsonrpc":"2.0","id":99,"method":"ping"}\n');
      } else {
        f.stdin.write(Buffer.alloc(10 * 1024 * 1024 + 1, 0x20));
      }
      await f.stdin.flush();
      const ended = await Promise.race([f.exited, Bun.sleep(5_000).then(() => 'running')]);
      expect(ended).toBe(0);
      expect(f.paths(from)).toEqual([USER]);
    } finally {
      // A serve still closing would wait out the stalled tick; SIGTERM does not stop a close.
      f.kill();
      await f.close();
    }
  }
});
