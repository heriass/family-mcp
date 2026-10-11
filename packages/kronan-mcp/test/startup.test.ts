import { expect, test } from 'bun:test';
import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  StoreRefusal,
  checkSecretStore,
  defaultKeyProvider,
  defaultSecretRecordPath,
} from '@family-mcp/session-store';

import { migrateToken } from '../src/auth.js';

const cli = fileURLToPath(new URL('../src/cli.ts', import.meta.url));

const E3 = 'Other users can open this store folder.';

/** Run the CLI against a scratch store (through the store test seam); stdin is closed. */
async function run(home: string, args: string[]) {
  const child = Bun.spawn([process.execPath, cli, ...args], {
    env: {
      ...process.env,
      FAMILY_MCP_STORE_TEST_SEAM: '1',
      HOME: home,
      XDG_CONFIG_HOME: join(home, 'config'),
      XDG_DATA_HOME: join(home, 'data'),
    },
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
    timeout: 10_000,
  });

  const [stdout, stderr, status] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);

  return { stdout, stderr, status };
}

test('the CLI refuses to start on an unsafe store and touches nothing', async () => {
  const home = await mkdtemp(join(tmpdir(), 'kronan-mcp-startup-'));

  try {
    for (const directory of [
      join(home, 'config', 'kronan-mcp'),
      join(home, 'data', 'family-mcp', 'keys'),
    ]) {
      await mkdir(directory, { recursive: true, mode: 0o700 });
      await chmod(directory, 0o755);

      for (const args of [['serve'], ['auth', 'logout']]) {
        const result = await run(home, args);
        expect(result).toEqual({
          stdout: '',
          stderr: `kronan-mcp: cannot start. ${E3}\n  Path: '${directory}'\n  Fix:  chmod 700 '${directory}'\n`,
          status: 1,
        });
        expect(await readdir(directory)).toEqual([]);
      }

      // Help and version never touch the store.
      for (const flag of ['--help', '--version']) expect((await run(home, [flag])).status).toBe(0);
      await chmod(directory, 0o700);
    }
  } finally {
    await rm(home, { recursive: true, force: true });
  }
}, 60_000);

/**
 * Create the record folder and wait until Time Machine confirms its exclusion, as the store would.
 * A loaded machine's `tmutil isexcluded` can outlast the store's 5 s bound, which refuses the
 * store; once this process confirmed the folder, the store does not ask tmutil again for it.
 */
async function confirmExcluded(): Promise<void> {
  const path = defaultSecretRecordPath('kronan-mcp');
  const keys = defaultKeyProvider({ server: 'kronan-mcp', profile: 'default' });
  const deadline = Date.now() + 90_000;
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });

  for (;;)
    try {
      await checkSecretStore({ path, keys });

      return;
    } catch (error) {
      const unconfirmed =
        error instanceof StoreRefusal && error.message.startsWith('Time Machine did not confirm');

      if (!unconfirmed || Date.now() > deadline) throw error;
    }
}

test.skipIf(process.platform !== 'darwin')(
  'an earlier build’s macOS store keeps deciding, so no plaintext is imported over it',
  async () => {
    const home = await mkdtemp(join(tmpdir(), 'kronan-mcp-transition-'));
    const legacy = join(home, 'legacy-session.json');

    // The production macOS layout under a scratch HOME: the test seam is off for this test.
    const variables = {
      FAMILY_MCP_STORE_TEST_SEAM: undefined,
      HOME: home,
      XDG_CONFIG_HOME: undefined,
      XDG_DATA_HOME: undefined,
      KRONAN_TOKEN_FILE: legacy,
    };

    const saved = Object.fromEntries(
      Object.keys(variables).map((name) => [name, process.env[name]]),
    );

    try {
      for (const [name, value] of Object.entries(variables))
        if (value === undefined) delete process.env[name];
        else process.env[name] = value;
      await mkdir(join(home, '.config', 'kronan-mcp'), { recursive: true, mode: 0o700 });
      await writeFile(join(home, '.config', 'kronan-mcp', 'session.enc.marker'), 'old', {
        mode: 0o600,
      });
      await writeFile(legacy, '{}', { mode: 0o600 });
      await confirmExcluded();

      await assert.rejects(migrateToken(), /store key is missing/);
      expect(await readFile(legacy, 'utf8')).toBe('{}');
      expect(
        await readFile(join(home, '.config', 'kronan-mcp', 'session.enc.marker'), 'utf8'),
      ).toBe('old');
    } finally {
      for (const [name, value] of Object.entries(saved))
        if (value === undefined) delete process.env[name];
        else process.env[name] = value;
      await rm(home, { recursive: true, force: true });
    }
  },
  120_000,
);
