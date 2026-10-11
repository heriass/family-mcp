// A copy of packages/kronan-mcp/test/integration.test.ts run against the Rust binary through the
// drop-ins in rust-kronan.ts, changed only where marked `Rust:`. Cases arrive with the tools they
// exercise.
import { afterAll, test, expect } from 'bun:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import {
  chmod,
  mkdir,
  mkdtemp,
  readdir,
  readFile,
  rm,
  stat,
  symlink,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';

import {
  LocalKeyFileProvider,
  createSecretKey,
  resetSecretStore,
  writePrivateFile,
  type KeyProvider,
  type SecretRecordOptions,
} from '@family-mcp/session-store';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

import manifest from '../../../../packages/kronan-mcp/package.json' with { type: 'json' };
// Rust: the TypeScript record and migration, for the cases that check interop with them.
import { readAttempts } from '../../../../packages/kronan-mcp/src/attempts.ts';
import { migrateToken as typescriptMigrateToken } from '../../../../packages/kronan-mcp/src/auth.ts';

// Rust: the fixtures are shared with parity.ts.
import {
  CHECKOUT_TOKEN,
  CHECKOUT_TOTAL,
  LIST_TOKEN,
  NOTE_LINE_TOKEN,
  ORDER_TOKEN,
  UPSTREAM_EXTRA,
  activeOrder,
  approval,
  checkout,
  fixtureResponse,
  order,
  product,
  recipe,
} from './fixtures.ts';
// Rust: the client, the auth functions, the schemas and the executable are the binary.
import {
  addCheckoutToOrderInput,
  addShoppingNoteLinesInput,
  attemptsPath,
  categoryProductsInput,
  changeShoppingNoteLineInput,
  clearShoppingNoteInput,
  completeCheckoutInput,
  deleteOrderLinesInput,
  emptyInput,
  failing,
  getProductInput,
  KeyFile,
  KronanClient,
  listOrdersInput,
  loadSavedToken,
  loadToken,
  logoutToken,
  lookupProductsInput,
  lowerOrderLineQuantitiesInput,
  migrateToken,
  normalizeToken,
  offsetInput,
  onPreloadOutput,
  orderInput,
  pageInput,
  preloadUpstream,
  previewCheckoutLinesInput,
  productListInput,
  productsByTagInput,
  purchaseStatsInput,
  recipeInput,
  reserveDeliverySlotInput,
  reservePickupSlotInput,
  saveToken,
  searchProductsInput,
  searchRecipesInput,
  serveCommand,
  setCheckoutLinesInput,
  shoppingNoteLineTokenInput,
  spawnCli,
  summarizeOrderLinesInput,
  toggleOrderLineSubstitutionInput,
  upstream,
} from './rust-kronan.ts';
// Rust: `VERSION` from src/server.ts is the package version.
const VERSION = manifest.version;

const TOKEN = 'synthetic-token-0123456789';

/** Order-attempt records from this file stay in a private scratch directory. */
const SCRATCH = await mkdtemp(join(tmpdir(), 'kronan-attempts-'));

// A client built with the default attempts path, and the store and its key, must never touch
// the real configuration. CLI child processes inherit these too.
process.env.KRONAN_TOKEN_FILE = join(SCRATCH, 'default-session.json');

process.env.XDG_CONFIG_HOME = join(SCRATCH, 'config');

process.env.XDG_DATA_HOME = join(SCRATCH, 'data');

/** A configuration directory of its own beside a legacy token file, outside its name space. */
const configFor = (legacy: string) => join(dirname(legacy), `config-${basename(legacy)}`);

/** The encrypted record under an `XDG_CONFIG_HOME`. */
const recordIn = (config: string) => join(config, 'kronan-mcp', 'session.enc');

/** Point the legacy token file and the store's configuration directory at scratch paths. */
async function withTokenFiles<T>(
  legacy: string,
  work: () => Promise<T>,
  config = configFor(legacy),
): Promise<T> {
  const previous = [process.env.KRONAN_TOKEN_FILE, process.env.XDG_CONFIG_HOME];
  process.env.KRONAN_TOKEN_FILE = legacy;
  process.env.XDG_CONFIG_HOME = config;

  try {
    return await work();
  } finally {
    [process.env.KRONAN_TOKEN_FILE, process.env.XDG_CONFIG_HOME] = previous;
  }
}

/** The plaintext token file that versions before the encrypted store wrote. */
const writeLegacyToken = (path: string, token: string) =>
  writePrivateFile(path, JSON.stringify({ version: 1, token }) + '\n');

afterAll(() => rm(SCRATCH, { recursive: true, force: true }));

// Rust: the binary keeps its record beside its token file, named for it.
const scratchFile = () => join(SCRATCH, randomUUID() + '.json.order-attempts.json');

type CapturedRequest = {
  url: string;
  method: string | undefined;
  headers: Headers;
  body: string | undefined;
  redirect: RequestRedirect | undefined;
};

type ContractCase = {
  name: string;
  method: 'GET' | 'POST' | 'PATCH' | 'DELETE';
  path: string;
  query?: string;
  body?: string;
  /** Reads the money gate sends first, as 'METHOD /path/'. */
  gate?: string[];
  call: () => Promise<object>;
};

const CHECKOUT_GATE = ['GET /checkout/'];

async function captureFailure(
  messages: string[],
  run: () => Promise<object>,
  pattern: RegExp,
): Promise<void> {
  try {
    await run();
  } catch (cause) {
    assert(cause instanceof Error);
    messages.push(cause.message);
    expect(cause.message).toMatch(pattern);

    return;
  }

  assert.fail('Expected the operation to fail.');
}

function withUnexpectedKey<T extends object>(input: T) {
  return { ...input, unexpected: TOKEN };
}

test('offset pages report nextOffset from the returned item count, not the requested limit', async () => {
  const api = new KronanClient(
    async () => TOKEN,
    async () =>
      Response.json({
        count: 60,
        next: 'https://api.kronan.is/api/v1/recipes/?limit=50&offset=50',
        previous: null,
        results: [recipe, recipe],
      }),
  );

  try {
    // Upstream clamped a 100-item request to two items; the next window must start after them.
    const page = await api.recipes({ limit: 100, offset: 10 });
    expect(page).toMatchObject({
      count: 60,
      limit: 100,
      offset: 10,
      hasNextPage: true,
      nextOffset: 12,
    });
    expect(page.results).toHaveLength(2);

    const last = new KronanClient(
      async () => TOKEN,
      async () => Response.json({ count: 2, next: null, previous: null, results: [recipe] }),
    );

    expect(await last.favoriteRecipes({ offset: 1 })).toMatchObject({
      hasNextPage: false,
      nextOffset: null,
    });
    await last.close();
  } finally {
    await api.close();
  }
});

test('oversized token files and standard input are rejected with a size message', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-size-'));

  try {
    const huge = join(directory, 'huge.json');
    await writeFile(huge, JSON.stringify({ version: 1, token: 'x'.repeat(20_000) }), {
      mode: 0o600,
    });
    await chmod(huge, 0o600);
    await withTokenFiles(huge, () => assert.rejects(loadToken(), /too large to be a token file/));

    const oversized = await runCli(['auth', 'set', '-'], {
      tokenFile: join(directory, 'saved.json'),
      input: 'y'.repeat(20_000),
    });

    expect(oversized.exitCode).toBe(1);
    expect(oversized.stderr).toMatch(/too large to hold one access token/);
    await assert.rejects(stat(recordIn(configFor(join(directory, 'saved.json')))), {
      code: 'ENOENT',
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('legacy token file permissions and parsing, and token normalization', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-token-test-'));
  const path = join(directory, 'session.json');
  const loosePath = join(directory, 'loose.json');
  const linkPath = join(directory, 'link.json');
  const invalidPath = join(directory, 'invalid.json');
  const load = (legacy: string) => withTokenFiles(legacy, () => loadToken());

  try {
    await writeLegacyToken(path, TOKEN);
    expect(await load(path)).toBe(TOKEN);

    if (process.platform !== 'win32') {
      await writeFile(loosePath, JSON.stringify({ version: 1, token: TOKEN }), { mode: 0o644 });
      await chmod(loosePath, 0o644);
      await assert.rejects(load(loosePath), /Cannot read the Krónan token file/);
      await symlink(path, linkPath);
      await assert.rejects(load(linkPath), /Cannot read the Krónan token file/);
    }

    await writeFile(invalidPath, '{"version":2,"token":"synthetic-token-0123456789"}', {
      mode: 0o600,
    });
    await assert.rejects(load(invalidPath), /Invalid Krónan token file/);
    await assert.rejects(load(join(directory, 'missing.json')), /No saved Krónan access token/);

    // Rust: the binary normalizes, so each check is awaited.
    expect(await normalizeToken('  ' + TOKEN + ' \n')).toBe(TOKEN);
    await expect(normalizeToken('')).rejects.toThrow(/Invalid Krónan access token/);
    await expect(normalizeToken('short')).rejects.toThrow(/Invalid Krónan access token/);
    await expect(normalizeToken('synthetic\r\ntoken')).rejects.toThrow(/Invalid Krónan access token/);
    await expect(normalizeToken('synthetic-tökén')).rejects.toThrow(/Invalid Krónan access token/);
    await expect(normalizeToken('x'.repeat(4097))).rejects.toThrow(/Invalid Krónan access token/);

    await withTokenFiles(join(directory, 'already-missing.json'), () => logoutToken());
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('migrate moves the legacy token once; afterwards only the store is read', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-migrate-'));
  const legacy = join(directory, 'session.json');
  const config = join(directory, 'store');
  const secret = recordIn(config);
  // Rust: the binary reads only the default key file.
  const keys = new KeyFile();
  const attempts = `${legacy}.order-attempts.json`;
  const other = 'synthetic-token-planted-later';

  try {
    await withTokenFiles(
      legacy,
      async () => {
        await assert.rejects(
          loadToken(keys),
          /No saved Krónan access token. Run kronan-mcp auth set/,
        );
        await assert.rejects(migrateToken(keys), /No saved Krónan access token/);

        await writeLegacyToken(legacy, TOKEN);
        await writePrivateFile(attempts, 'journal bytes stay as they are');
        expect(attemptsPath()).toBe(attempts);

        expect(await loadSavedToken(keys)).toEqual({
          token: TOKEN,
          storage: 'Saved in a plaintext file. Run kronan-mcp auth migrate.',
        });

        expect(await migrateToken(keys)).toBe('migrated');
        await assert.rejects(stat(legacy), { code: 'ENOENT' });
        expect(await readFile(secret, 'utf8')).not.toContain(TOKEN);
        expect(await readFile(`${secret}.marker`, 'utf8')).not.toContain(TOKEN);

        expect(await loadSavedToken(keys)).toEqual({
          token: TOKEN,
          storage: 'Saved in an encrypted file.',
        });

        expect(await migrateToken(keys)).toBe('already');

        // A legacy file that reappears after migration is never read; migrate removes it.
        await writeLegacyToken(legacy, other);
        expect(await loadToken(keys)).toBe(TOKEN);
        expect(await migrateToken(keys)).toBe('already-removed-legacy');
        await assert.rejects(stat(legacy), { code: 'ENOENT' });

        await writeLegacyToken(legacy, other);
        await logoutToken(keys);
        await assert.rejects(stat(legacy), { code: 'ENOENT' });
        await stat(secret);
        await assert.rejects(loadToken(keys), /No saved Krónan access token/);
        await writeLegacyToken(legacy, other);
        await assert.rejects(loadToken(keys), /No saved Krónan access token/);
        expect(await migrateToken(keys)).toBe('already-removed-legacy');

        expect(await saveToken(TOKEN, keys)).toBe(false);
        expect(await loadToken(keys)).toBe(TOKEN);

        // The order-attempt journal keeps its path and bytes through all of it.
        expect(attemptsPath()).toBe(attempts);
        expect(await readFile(attempts, 'utf8')).toBe('journal bytes stay as they are');
      },
      config,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('store key failures fail closed with fixed messages and never fall back or reset', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-store-errors-'));
  const legacy = join(directory, 'session.json');
  const config = join(directory, 'config');
  const secret = recordIn(config);
  // Rust: the binary reads only the default key file.
  const keys = new KeyFile();
  const keyPath = keys.path;

  const snapshot = async () =>
    Promise.all([readFile(secret), readFile(`${secret}.marker`), readFile(keyPath)]);

  try {
    await withTokenFiles(
      legacy,
      async () => {
        expect(await saveToken(TOKEN, keys)).toBe(false);
        expect((await stat(keyPath)).mode & 0o777).toBe(0o600);
        const before = await snapshot();
        await writeLegacyToken(legacy, 'synthetic-token-stale-plaintext');
        const messages: string[] = [];

        // Rust: UNSAFE_FILE, STORE_BACKEND_RETIRED and STORE_LOCKED from an injected provider cannot
        // reach the binary; src/auth.rs tests their messages.
        for (const [code, pattern] of [['STORE_ERROR', /Cannot use the Krónan token store/]] as const) {
          for (const work of [
            () => loadToken(failing(code)),
            () => saveToken(TOKEN, failing(code)),
            () => migrateToken(failing(code)),
            () => logoutToken(failing(code)),
          ]) {
            const error = await work().then(
              () => assert.fail(`${code} was accepted.`),
              (cause: unknown) => cause,
            );

            assert(error instanceof Error);
            expect(error.message).toMatch(pattern);
            messages.push(error.message);
          }

          // Nothing was reset, rewritten, or read from the plaintext file instead.
          expect(await snapshot()).toEqual(before);
          await stat(legacy);
        }

        // A wrong key is an authentication failure, not a missing key: auth set refuses too.
        // Rust: a key file of its own holds the wrong key.
        const wrong = new KeyFile(new Uint8Array(32).fill(9));
        await assert.rejects(loadToken(wrong), /Cannot use the Krónan token store/);
        await assert.rejects(saveToken(TOKEN, wrong), /Cannot use the Krónan token store/);
        expect(await snapshot()).toEqual(before);

        // A record without its marker is an interrupted write; its token is not used.
        const marker = await readFile(`${secret}.marker`);
        await rm(`${secret}.marker`);
        await assert.rejects(loadToken(keys), /did not complete, so its token is not used/);
        await writePrivateFile(`${secret}.marker`, marker);

        // A deleted key fails closed everywhere except an explicit new auth set.
        await rm(keyPath);

        const missing =
          /The Krónan store key is missing\. Run kronan-mcp auth set to save the token again\.$/;

        let requests = 0;

        const client = new KronanClient(
          () => loadToken(keys),
          async () => {
            requests++;

            return Response.json({ type: 'user', name: 'Test' });
          },
          join(directory, 'attempts.json'),
        );

        try {
          await assert.rejects(client.status(), missing);
        } finally {
          await client.close();
        }

        expect(requests).toBe(0);
        await assert.rejects(loadSavedToken(keys), missing);
        await assert.rejects(migrateToken(keys), missing);
        await assert.rejects(logoutToken(keys), missing);
        await assert.rejects(stat(keyPath), { code: 'ENOENT' });
        await stat(secret);
        expect(await saveToken(TOKEN, keys)).toBe(true);
        expect(await loadToken(keys)).toBe(TOKEN);
        await assert.rejects(stat(legacy), { code: 'ENOENT' });

        for (const message of messages) {
          expect(message).not.toContain(TOKEN);
          expect(message).not.toContain(directory);
        }
      },
      config,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

/** The options auth.ts uses, so a test can stop a recovery at an exact step. */
const kronanRecord = (config: string, keys: KeyProvider): SecretRecordOptions => ({
  path: recordIn(config),
  server: 'kronan-mcp',
  profile: 'default',
  purpose: 'token',
  schema: 1,
  maxBytes: 16_384,
  keys,
});

test('a store with a marker never falls back to the legacy file after any crash point', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-crash-'));
  const legacy = join(directory, 'session.json');
  const config = join(directory, 'config');
  // Rust: the binary reads only the default key file.
  const keys = new KeyFile();
  const keyPath = keys.path;
  const record = kronanRecord(config, keys);
  const stale = 'synthetic-token-stale-plaintext';
  const noToken = /No saved Krónan access token\. Run kronan-mcp auth set first\./;

  try {
    await withTokenFiles(
      legacy,
      async () => {
        await saveToken(TOKEN, keys);
        await writeLegacyToken(legacy, stale);
        await rm(keyPath);

        // Crash after the reset: a fresh marker, no record, no key.
        await resetSecretStore(record);
        await assert.rejects(loadToken(keys), /The Krónan store key is missing/);
        await stat(legacy);

        // Crash after the new key was created, before the write.
        await createSecretKey(record);
        await assert.rejects(loadSavedToken(keys), noToken);
        await assert.rejects(loadToken(keys), noToken);
        await logoutToken(keys);
        await assert.rejects(stat(legacy), { code: 'ENOENT' });
        await assert.rejects(loadToken(keys), noToken);

        // A first write that stopped after its pending marker, with a legacy token present.
        await rm(recordIn(config));
        await writePrivateFile(
          `${recordIn(config)}.marker`,
          '{"backend":"encrypted-file","keySource":"local-file","keyId":"local","profile":"default","migrated":false,"generation":0,"pending":{"generation":1,"nonce":"AAAAAAAAAAAAAAAA"}}\n',
        );
        await writeLegacyToken(legacy, TOKEN);
        await assert.rejects(loadToken(keys), noToken);
        await assert.rejects(loadSavedToken(keys), noToken);

        // Only the explicit migration resumes it.
        expect(await migrateToken(keys)).toBe('migrated');
        expect(await loadToken(keys)).toBe(TOKEN);
        await assert.rejects(stat(legacy), { code: 'ENOENT' });
      },
      config,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('a legacy path that aliases the store, its key, or their lock and temporaries is refused', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-collide-'));
  const config = join(directory, 'config');
  const store = join(config, 'kronan-mcp');
  // Rust: the binary reads only the default key file.
  const keys = new KeyFile();
  const keyPath = keys.path;
  const alias = join(directory, 'alias');

  try {
    await withTokenFiles(join(directory, 'session.json'), () => saveToken(TOKEN, keys), config);
    await rm(keyPath);
    await symlink(store, alias);

    const files = [
      recordIn(config),
      `${recordIn(config)}.marker`,
      // Rust: the key file is the binary's default one.
      keyPath,
    ];

    const snapshot = () => Promise.all(files.map((file) => readFile(file).catch(() => 'absent')));

    const before = await snapshot();

    for (const legacy of [
      recordIn(config),
      `${recordIn(config)}.marker`,
      `${recordIn(config)}.lock`,
      `${recordIn(config)}.0123.tmp`,
      join(store, 'session'),
      join(alias, 'session.enc'),
      join(alias, 'session.enc.marker'),
      join(store, 'nested', '..', 'session.enc'),
      keyPath,
      // Rust: the key file is the binary's default one.
      join(dirname(keyPath), '.', `${basename(keyPath)}.lock`),
    ]) {
      // An unresolved order journal beside the configured path keeps its bytes.
      const journal = `${legacy}.order-attempts.json`;
      await writePrivateFile(journal, 'unresolved journal');

      await withTokenFiles(
        legacy,
        async () => {
          for (const work of [
            () => saveToken(TOKEN, keys),
            () => migrateToken(keys),
            () => logoutToken(keys),
          ])
            await assert.rejects(
              work(),
              /KRONAN_TOKEN_FILE overlaps the encrypted Krónan token store/,
            );
        },
        config,
      );

      expect(await readFile(journal, 'utf8')).toBe('unresolved journal');
      await rm(journal);
      expect(await snapshot()).toEqual(before);
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('logout waits for an in-flight migration instead of being undone by it', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-interleave-'));
  const legacy = join(directory, 'session.json');
  const config = join(directory, 'config');
  const key = new Uint8Array(32).fill(5);
  const entered = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();

  // Rust: the key is a key file both the TypeScript migration and the binary read.
  const keyFile = new KeyFile(key);

  /** Pauses migration inside its locks, after it read the legacy file. */
  class PausedKeys extends LocalKeyFileProvider {
    override async getKey(): Promise<Uint8Array> {
      entered.resolve();
      await release.promise;

      return super.getKey();
    }
  }

  try {
    await withTokenFiles(
      legacy,
      async () => {
        await writeLegacyToken(legacy, TOKEN);
        // Rust: the TypeScript migration holds the locks while the binary's logout waits for them.
        const migrating = typescriptMigrateToken(new PausedKeys({ path: keyFile.path }));
        await entered.promise;
        let loggedOut = false;

        const logout = logoutToken(keyFile).finally(() => {
          loggedOut = true;
        });

        // Longer than the lock's longest poll interval: logout must still be waiting.
        await Bun.sleep(600);
        expect(loggedOut).toBe(false);
        release.resolve();
        expect(await migrating).toBe('migrated');
        await logout;
        await assert.rejects(stat(legacy), { code: 'ENOENT' });
        await assert.rejects(loadToken(keyFile), /No saved Krónan access token/);
      },
      config,
    );
  } finally {
    release.resolve();
    await rm(directory, { recursive: true, force: true });
  }
});

test('a legacy file that cannot be removed is a fixed message after the token was saved', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-cleanup-'));
  const legacy = join(directory, 'session.json');
  // Rust: the binary reads only the default key file.
  const keys = new KeyFile();

  try {
    await mkdir(join(legacy, 'occupied'), { recursive: true });

    await withTokenFiles(legacy, async () => {
      const error = await saveToken(TOKEN, keys).then(
        () => assert.fail('A directory at the legacy path was removed.'),
        (cause: unknown) => cause,
      );

      assert(error instanceof Error);
      expect(error.message).toBe(
        'Saved in the encrypted store, but the old plaintext token file could not be removed. Remove it by hand.',
      );
      expect(await loadToken(keys)).toBe(TOKEN);
      await stat(join(legacy, 'occupied'));
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('all public client methods use the exact published paths, parameters, bodies, and outputs', async () => {
  const requests: CapturedRequest[] = [];
  const attempts = scratchFile();

  const client = new KronanClient(
    async () => TOKEN,
    async (url, options) => {
      requests.push({
        url,
        method: options.method,
        headers: new Headers(options.headers),
        body:
          options.body === undefined || options.body === null
            ? undefined
            : await new Response(options.body).text(),
        redirect: options.redirect,
      });

      return fixtureResponse(new URL(url).pathname);
    },
    attempts,
  );

  const cases: ContractCase[] = [
    { name: 'status', method: 'GET', path: '/me/', call: () => client.status() },
    {
      name: 'searchProducts',
      method: 'POST',
      path: '/products/search/',
      body: JSON.stringify({
        query: 'milk',
        page: 1,
        pageSize: 20,
        withDetail: false,
        includePurchaseHistory: false,
      }),
      call: () => client.searchProducts({ query: 'milk' }),
    },
    {
      name: 'product by SKU',
      method: 'GET',
      path: '/products/SKU-1/',
      call: () => client.product({ sku: 'SKU-1' }),
    },
    {
      name: 'product by barcode',
      method: 'GET',
      path: '/products/barcode/12345678/',
      call: () => client.product({ barcode: '12345678' }),
    },
    {
      name: 'lookupProducts',
      method: 'POST',
      path: '/products/batch/',
      body: JSON.stringify({ skus: ['SKU-1', 'SKU-2'] }),
      call: () => client.lookupProducts({ skus: ['SKU-1', 'SKU-2'] }),
    },
    { name: 'categories', method: 'GET', path: '/categories/', call: () => client.categories() },
    {
      name: 'categoryProducts',
      method: 'GET',
      path: '/categories/dairy/products/',
      query: 'page=2',
      call: () => client.categoryProducts({ slug: 'dairy', page: 2 }),
    },
    { name: 'tags', method: 'GET', path: '/products/tags/', call: () => client.tags() },
    {
      name: 'productsByTag',
      method: 'GET',
      path: '/products/by-tag/vegan/',
      query: 'page=3',
      call: () => client.productsByTag({ slug: 'vegan', page: 3 }),
    },
    {
      name: 'productsOnSale',
      method: 'GET',
      path: '/products/on-sale/',
      query: 'page=4',
      call: () => client.productsOnSale({ page: 4 }),
    },
    {
      name: 'favoriteProducts',
      method: 'GET',
      path: '/products/favorites/',
      query: 'page=5',
      call: () => client.favoriteProducts({ page: 5 }),
    },
    {
      name: 'orders',
      method: 'GET',
      path: '/orders/',
      query: 'limit=20&offset=0&year=2025&month=8&type=delivery',
      call: () => client.orders({ year: 2025, month: 8, type: 'delivery' }),
    },
    {
      name: 'order',
      method: 'GET',
      path: '/orders/' + ORDER_TOKEN + '/',
      call: () => client.order({ token: ORDER_TOKEN }),
    },
    {
      name: 'activeOrder',
      method: 'GET',
      path: '/orders/currently-active/',
      call: () => client.activeOrder(),
    },
    {
      name: 'orderLineSummary',
      method: 'GET',
      path: '/orders/line-summary/',
      query: 'from_year=2025&from_month=1&to_year=2025&to_month=6&skus=SKU-1&skus=SKU-2',
      call: () =>
        client.orderLineSummary({
          fromYear: 2025,
          fromMonth: 1,
          toYear: 2025,
          toMonth: 6,
          skus: ['SKU-1', 'SKU-2'],
        }),
    },
    {
      name: 'orderLineSummary name filter',
      method: 'GET',
      path: '/orders/line-summary/',
      query: 'name_contains=milk',
      call: () => client.orderLineSummary({ nameContains: 'milk' }),
    },
    {
      name: 'purchaseStats',
      method: 'GET',
      path: '/product-purchase-stats/',
      query: 'limit=7&offset=3&sort=most_quantity&include_ignored=true',
      call: () =>
        client.purchaseStats({ limit: 7, offset: 3, sort: 'most_quantity', includeIgnored: true }),
    },
    {
      name: 'shoppingNote',
      method: 'GET',
      path: '/shopping-notes/',
      call: () => client.shoppingNote(),
    },
    {
      name: 'archivedShoppingNoteLines',
      method: 'GET',
      path: '/shopping-notes/lines-archived/',
      call: () => client.archivedShoppingNoteLines(),
    },
    {
      name: 'productLists',
      method: 'GET',
      path: '/product-lists/',
      query: 'limit=20&offset=0',
      call: () => client.productLists(),
    },
    {
      name: 'productList',
      method: 'GET',
      path: '/product-lists/' + LIST_TOKEN + '/',
      call: () => client.productList({ token: LIST_TOKEN }),
    },
    {
      name: 'recipes',
      method: 'GET',
      path: '/recipes/',
      query: 'limit=20&offset=0',
      call: () => client.recipes(),
    },
    {
      name: 'searchRecipes',
      method: 'POST',
      path: '/recipes/search/',
      body: JSON.stringify({
        query: 'oats',
        tags: [1],
        ingredientTags: [],
        cuisineTags: [],
        occasionTags: [],
        page: 1,
        orderBy: 'default',
      }),
      call: () => client.searchRecipes({ query: 'oats', tags: [1] }),
    },
    {
      name: 'recipe',
      method: 'GET',
      path: '/recipes/oat-cakes/',
      call: () => client.recipe({ slug: 'oat-cakes' }),
    },
    {
      name: 'favoriteRecipes',
      method: 'GET',
      path: '/recipes/favorites/',
      query: 'limit=20&offset=0',
      call: () => client.favoriteRecipes(),
    },
    {
      name: 'addresses',
      method: 'GET',
      path: '/addresses/',
      call: () => client.addresses(),
    },
    {
      name: 'deliverySlots',
      method: 'POST',
      path: '/slots/delivery/',
      body: JSON.stringify({ addressId: 11 }),
      call: () => client.deliverySlots({ addressId: 11 }),
    },
    {
      name: 'pickupSlots',
      method: 'POST',
      path: '/slots/pickup/',
      body: JSON.stringify({ chain: 'pikkolo' }),
      call: () => client.pickupSlots({ chain: 'pikkolo' }),
    },
    {
      name: 'checkout',
      method: 'GET',
      path: '/checkout/',
      call: () => client.checkout(),
    },
    {
      name: 'addShoppingNoteLines',
      method: 'POST',
      path: '/shopping-notes/add-lines/',
      body: JSON.stringify({ lines: [{ text: 'Eggs', quantity: 12 }, { sku: 'SKU-1' }] }),
      call: () =>
        client.addShoppingNoteLines({ lines: [{ text: 'Eggs', quantity: 12 }, { sku: 'SKU-1' }] }),
    },
    {
      name: 'changeShoppingNoteLine',
      method: 'PATCH',
      path: '/shopping-notes/change-line/',
      body: JSON.stringify({ token: NOTE_LINE_TOKEN, quantity: 3 }),
      call: () => client.changeShoppingNoteLine({ token: NOTE_LINE_TOKEN, quantity: 3 }),
    },
    {
      name: 'toggleShoppingNoteLineComplete',
      method: 'PATCH',
      path: '/shopping-notes/toggle-complete-on-line/',
      body: JSON.stringify({ token: NOTE_LINE_TOKEN }),
      call: () => client.toggleShoppingNoteLineComplete({ token: NOTE_LINE_TOKEN }),
    },
    {
      name: 'deleteShoppingNoteLine',
      method: 'DELETE',
      path: '/shopping-notes/delete-line/',
      query: 'token=' + NOTE_LINE_TOKEN,
      call: () => client.deleteShoppingNoteLine({ token: NOTE_LINE_TOKEN }),
    },
    {
      name: 'clearShoppingNote',
      method: 'DELETE',
      path: '/shopping-notes/delete-shopping-note/',
      call: () => client.clearShoppingNote({ confirm: true }),
    },
    {
      name: 'previewCheckoutLines',
      method: 'POST',
      path: '/checkout/preview-lines/',
      body: JSON.stringify({
        lines: [
          { sku: 'SKU-1', quantity: 2 },
          { sku: 'SKU-2', quantity: 1 },
        ],
      }),
      call: () =>
        client.previewCheckoutLines({ lines: [{ sku: 'SKU-1', quantity: 2 }, { sku: 'SKU-2' }] }),
    },
    {
      name: 'setCheckoutLines replace',
      method: 'POST',
      path: '/checkout/lines/',
      body: JSON.stringify({
        lines: [{ sku: 'SKU-1', quantity: 2, substitution: false }],
        replace: true,
      }),
      call: () =>
        client.setCheckoutLines({
          lines: [{ sku: 'SKU-1', quantity: 2, substitution: false }],
          replace: true,
        }),
    },
    {
      name: 'setCheckoutLines add',
      method: 'POST',
      path: '/checkout/lines/',
      body: JSON.stringify({ lines: [{ sku: 'SKU-1', quantity: 1 }], replace: false }),
      call: () => client.setCheckoutLines({ lines: [{ sku: 'SKU-1' }], replace: false }),
    },
    {
      name: 'reserveDeliverySlot',
      method: 'POST',
      path: '/slots/delivery/reserve/',
      gate: CHECKOUT_GATE,
      body: JSON.stringify({ slotId: 501, addressId: 11, returnBags: true }),
      call: () =>
        client.reserveDeliverySlot({ ...approval, slotId: 501, addressId: 11, returnBags: true }),
    },
    {
      name: 'reservePickupSlot',
      method: 'POST',
      path: '/slots/pickup/reserve/',
      gate: CHECKOUT_GATE,
      body: JSON.stringify({ slotId: 601, returnBags: false }),
      call: () => client.reservePickupSlot({ ...approval, slotId: 601, returnBags: false }),
    },
    {
      name: 'completeCheckout delivery',
      method: 'POST',
      path: '/checkout/complete/',
      gate: CHECKOUT_GATE,
      body: JSON.stringify({ slotId: 501, addressId: 11, returnBags: false }),
      call: () =>
        client.completeCheckout({ ...approval, slotId: 501, addressId: 11, returnBags: false }),
    },
    {
      name: 'completeCheckout pickup',
      method: 'POST',
      path: '/checkout/complete/',
      gate: CHECKOUT_GATE,
      body: JSON.stringify({ slotId: 601, returnBags: true }),
      call: () => client.completeCheckout({ ...approval, slotId: 601, returnBags: true }),
    },
    {
      name: 'addCheckoutToOrder',
      method: 'POST',
      path: '/checkout/add-to-order/',
      gate: [...CHECKOUT_GATE, 'GET /orders/currently-active/'],
      call: () => client.addCheckoutToOrder({ ...approval, expectedOrderToken: ORDER_TOKEN }),
    },
    {
      name: 'deleteOrderLines',
      method: 'POST',
      path: '/orders/' + ORDER_TOKEN + '/delete-lines/',
      body: JSON.stringify({ lineIds: [1, 2] }),
      call: () =>
        client.deleteOrderLines({ orderToken: ORDER_TOKEN, lineIds: [1, 2], confirm: true }),
    },
    {
      name: 'lowerOrderLineQuantities',
      method: 'POST',
      path: '/orders/' + ORDER_TOKEN + '/lower-quantity-lines/',
      body: JSON.stringify({ lineIds: [1], quantity: 0 }),
      call: () =>
        client.lowerOrderLineQuantities({
          orderToken: ORDER_TOKEN,
          lineIds: [1],
          quantity: 0,
          confirm: true,
        }),
    },
    {
      name: 'toggleOrderLineSubstitution',
      method: 'POST',
      path: '/orders/' + ORDER_TOKEN + '/lines-toggle-substitution/',
      body: JSON.stringify({ lineIds: [1] }),
      call: () => client.toggleOrderLineSubstitution({ orderToken: ORDER_TOKEN, lineIds: [1] }),
    },
  ];

  try {
    for (const item of cases) {
      // Each money case is its own approval; the attempts rules have their own tests.
      await rm(attempts, { force: true });
      const before = requests.length;
      const output = await item.call();
      const sent = requests.slice(before);
      const request = sent.at(-1);
      assert(request);

      // Exactly one request per call beyond the documented gate reads; nothing is retried.
      expect(
        sent.map(
          (entry) => entry.method + ' ' + new URL(entry.url).pathname.slice('/api/v1'.length),
        ),
        item.name,
      ).toEqual([...(item.gate ?? []), item.method + ' ' + item.path]);

      expect(request.url).toBe(
        'https://api.kronan.is/api/v1' +
          item.path +
          (item.query === undefined ? '' : '?' + item.query),
      );
      expect(request.method).toBe(item.method);
      expect(request.headers.get('authorization')).toBe('AccessToken ' + TOKEN);
      expect(request.headers.get('accept')).toBe('application/json');
      expect(request.headers.get('content-type')).toBe(
        item.body === undefined ? null : 'application/json',
      );
      expect(request.redirect).toBe('error');
      expect(request.body).toBe(item.body);
      expect(JSON.stringify(output)).not.toContain(UPSTREAM_EXTRA);
    }
  } finally {
    await client.close();
  }
});

type Scripted = (
  pathname: string,
  method: string,
) => Response | undefined | Promise<Response | undefined>;

/** A client whose requests are recorded; script overrides the fixture for chosen requests. */
function scriptedClient(script: Scripted, attempts = scratchFile()) {
  const sent: string[] = [];

  const client = new KronanClient(
    async () => TOKEN,
    async (url, options) => {
      const { pathname } = new URL(url);
      const method = options.method ?? 'GET';
      sent.push(method + ' ' + pathname.slice('/api/v1'.length));

      return (await script(pathname, method)) ?? fixtureResponse(pathname);
    },
    attempts,
  );

  return { client, sent, attempts };
}

const posts = (entries: string[]) => entries.filter((entry) => entry.startsWith('POST'));

const NOTHING_SENT = /Nothing was sent to Krónan/;

const OUTCOME_UNKNOWN = /Outcome unknown.*do not retry or place another order/;

const placements = [
  {
    name: 'reserve_delivery_slot',
    path: '/slots/delivery/reserve/',
    call: (client: KronanClient, gate: typeof approval) =>
      client.reserveDeliverySlot({ ...gate, slotId: 501, addressId: 11, returnBags: false }),
  },
  {
    name: 'reserve_pickup_slot',
    path: '/slots/pickup/reserve/',
    call: (client: KronanClient, gate: typeof approval) =>
      client.reservePickupSlot({ ...gate, slotId: 601, returnBags: false }),
  },
  {
    name: 'complete_checkout',
    path: '/checkout/complete/',
    call: (client: KronanClient, gate: typeof approval) =>
      client.completeCheckout({ ...gate, slotId: 501, addressId: 11, returnBags: false }),
  },
  {
    name: 'add_checkout_to_order',
    path: '/checkout/add-to-order/',
    call: (client: KronanClient, gate: typeof approval) =>
      client.addCheckoutToOrder({ ...gate, expectedOrderToken: ORDER_TOKEN }),
  },
];

test('the money gate refuses without sending a charge-bearing request', async () => {
  let current = checkout;

  const { client, sent, attempts } = scriptedClient((pathname) =>
    pathname === '/api/v1/checkout/' ? Response.json(current) : undefined,
  );

  const refusals = [
    {
      label: 'total mismatch',
      state: checkout,
      gate: { ...approval, expectedTotal: CHECKOUT_TOTAL - 1 },
      pattern: /total differs/,
    },
    {
      label: 'token mismatch',
      state: checkout,
      gate: { ...approval, expectedCheckoutToken: LIST_TOKEN },
      pattern: /checkout token differs/,
    },
    {
      label: 'empty checkout',
      state: { ...checkout, lines: [], total: 0 },
      gate: { ...approval, expectedTotal: 0 },
      pattern: /checkout is empty/,
    },
  ];

  try {
    for (const placement of placements) {
      for (const { label, state, gate, pattern } of refusals) {
        current = state;
        const before = sent.length;
        const messages: string[] = [];
        await captureFailure(messages, () => placement.call(client, gate), pattern);
        expect(messages[0], placement.name + ': ' + label).toMatch(NOTHING_SENT);
        expect(messages[0]).toMatch(/no order was placed/);
        expect(sent.slice(before), placement.name + ': ' + label).toEqual(['GET /checkout/']);
      }
    }

    expect(sent.some((entry) => entry.startsWith('POST'))).toBe(false);
    // A refused gate records nothing, so it never blocks a later, corrected approval.
    await assert.rejects(stat(attempts), { code: 'ENOENT' });
  } finally {
    await client.close();
  }
});

test('money and destructive tools called without confirm:true send nothing', async () => {
  const { client: api, sent } = scriptedClient(() => undefined);
  // Rust: the server is the binary, serving api's token, record and upstream over stdio.
  const server = await api.serve();
  const client = new Client({ name: 'kronan-confirm', version: VERSION });
  const clientTransport = server.transport;
  const { confirm: _approved, ...unconfirmed } = approval;

  const calls = [
    ['reserve_delivery_slot', { ...unconfirmed, slotId: 501, addressId: 11, returnBags: false }],
    ['reserve_pickup_slot', { ...unconfirmed, slotId: 601, returnBags: false }],
    ['complete_checkout', { ...unconfirmed, slotId: 501, returnBags: false }],
    ['add_checkout_to_order', { ...unconfirmed, expectedOrderToken: ORDER_TOKEN }],
    ['clear_shopping_note', {}],
    ['delete_order_lines', { orderToken: ORDER_TOKEN, lineIds: [1] }],
    ['lower_order_line_quantities', { orderToken: ORDER_TOKEN, lineIds: [1], quantity: 0 }],
  ] as const;

  try {
    await client.connect(clientTransport);

    for (const [name, args] of calls) {
      for (const confirm of [undefined, false, 'true']) {
        const outcome = await client.callTool({
          name,
          arguments: confirm === undefined ? args : { ...args, confirm },
        });

        expect(outcome.isError, name + ' ' + String(confirm)).toBe(true);
      }
    }

    expect(sent).toEqual([]);
  } finally {
    await client.close();
    await server.close();
    await api.close();
  }
});

test('add_checkout_to_order refuses when the active order is absent or differs', async () => {
  let active: Response | undefined;

  const { client, sent } = scriptedClient((pathname) =>
    pathname === '/api/v1/orders/currently-active/' ? active : undefined,
  );

  try {
    for (const [response, pattern] of [
      [new Response(null, { status: 404 }), /no active order/],
      [Response.json({ ...activeOrder, orderToken: LIST_TOKEN }), /active order differs/],
    ] as const) {
      active = response;
      const before = sent.length;
      const messages: string[] = [];
      await captureFailure(
        messages,
        () => client.addCheckoutToOrder({ ...approval, expectedOrderToken: ORDER_TOKEN }),
        pattern,
      );
      expect(messages[0]).toMatch(NOTHING_SENT);
      expect(sent.slice(before)).toEqual(['GET /checkout/', 'GET /orders/currently-active/']);
    }
  } finally {
    await client.close();
  }
});

test('ambiguous money outcomes are reported as unknown after exactly one request', async () => {
  let failure: () => Response | Promise<Response>;

  const { client, sent, attempts } = scriptedClient((_pathname, method) =>
    method === 'POST' ? failure() : undefined,
  );

  // The upstream may have accepted the request before any of these; none proves a refusal.
  const ambiguous: [string, () => Response | Promise<Response>][] = [
    ['network error', () => Promise.reject(new TypeError('socket hang up'))],
    ['timeout', () => Promise.reject(new DOMException('Timed out', 'TimeoutError'))],
    ['server error', () => new Response(TOKEN, { status: 502 })],
    ['unparsable body', () => new Response('{not json', { status: 201 })],
    ['undocumented body', () => Response.json({ orderToken: ORDER_TOKEN }, { status: 201 })],
    ['accepted then 400', () => new Response(TOKEN, { status: 400 })],
    ['accepted then 408', () => new Response(TOKEN, { status: 408 })],
    ['accepted then 409', () => new Response(TOKEN, { status: 409 })],
    ['accepted then 422', () => new Response(TOKEN, { status: 422 })],
    ['accepted then 429', () => new Response(TOKEN, { status: 429 })],
    ['accepted then 401', () => new Response(TOKEN, { status: 401 })],
  ];

  try {
    for (const placement of placements) {
      for (const [label, respond] of ambiguous) {
        // Each case is a fresh approval; the record rules are tested separately.
        await rm(attempts, { force: true });
        failure = respond;
        const before = sent.length;
        const outcome = await placement.call(client, approval);

        expect(outcome.outcome, placement.name + ': ' + label).toBe('unknown');
        expect(outcome.message).toMatch(OUTCOME_UNKNOWN);
        expect(JSON.stringify(outcome)).not.toContain(TOKEN);
        expect(posts(sent.slice(before))).toEqual(['POST ' + placement.path]);
        expect(outcome.message).not.toMatch(/Nothing was sent|no order was placed/);
      }
    }
  } finally {
    await client.close();
  }
});

test('an unknown money outcome reaches the MCP host as a validated result, not an error', async () => {
  const {
    client: api,
    sent,
    attempts,
  } = scriptedClient((_pathname, method) =>
    method === 'POST' ? new Response(TOKEN, { status: 502 }) : undefined,
  );

  // Rust: the server is the binary, serving api's token, record and upstream over stdio.
  const server = await api.serve();
  const client = new Client({ name: 'kronan-unknown', version: VERSION });
  const clientTransport = server.transport;

  const calls = [
    ['reserve_delivery_slot', { ...approval, slotId: 501, addressId: 11, returnBags: false }],
    ['reserve_pickup_slot', { ...approval, slotId: 601, returnBags: false }],
    ['complete_checkout', { ...approval, slotId: 501, returnBags: false }],
    ['add_checkout_to_order', { ...approval, expectedOrderToken: ORDER_TOKEN }],
  ] as const;

  try {
    await client.connect(clientTransport);

    for (const [name, args] of calls) {
      await rm(attempts, { force: true });
      const before = sent.length;
      const outcome = await client.callTool({ name, arguments: args });

      expect(outcome.isError, name).not.toBe(true);
      expect(outcome.structuredContent, name).toMatchObject({ outcome: 'unknown' });
      expect(JSON.stringify(outcome.structuredContent)).toMatch(OUTCOME_UNKNOWN);
      expect(JSON.stringify(outcome)).not.toContain(TOKEN);
      expect(sent.slice(before).filter((entry) => entry.startsWith('POST'))).toHaveLength(1);
    }
  } finally {
    await client.close();
    await server.close();
    await api.close();
  }
});

test('a failed gate read refuses the money call without sending it', async () => {
  const { client, sent } = scriptedClient((pathname) =>
    pathname === '/api/v1/checkout/' ? new Response(TOKEN, { status: 503 }) : undefined,
  );

  try {
    for (const placement of placements) {
      const messages: string[] = [];
      await captureFailure(messages, () => placement.call(client, approval), NOTHING_SENT);
      expect(messages[0]).toMatch(/Could not read the checkout/);
    }

    expect(sent.every((entry) => entry === 'GET /checkout/')).toBe(true);
  } finally {
    await client.close();
  }
});

// Rust: an intended difference from api.ts (api.rs `place`). The binary reads the saved token
// once, in the gate, and sends the order call with it. A token that cannot be read refuses with
// its own message before anything is recorded or sent; api.ts reads it inside the request, where
// the same failure records an unknown outcome for a request that never left.
test('a token that cannot be read refuses the money call before anything is recorded or sent', async () => {
  const unreadable =
    'Cannot read the Krónan token file. Use a regular file that you own with owner-only permissions (chmod 600 on Unix), not a symlink or hard link.';

  for (const placement of placements) {
    // The token file a group can read, named for the record (rust-kronan.ts): before the gate.
    const { client, sent, attempts } = scriptedClient(() => undefined);
    const token = attempts.slice(0, -'.order-attempts.json'.length);
    await writeFile(token, '', { mode: 0o600 });
    await chmod(token, 0o640);

    try {
      await assert.rejects(placement.call(client, approval), { message: unreadable });
      expect(sent, placement.name).toEqual([]);
      await assert.rejects(stat(attempts), { code: 'ENOENT' });
    } finally {
      await client.close();
    }

    // The same failure once the gate has read the live checkout: the order call is still sent
    // once, with the token the gate read, instead of an unknown outcome without a request.
    const later = scriptedClient(async (pathname, method) => {
      if (method === 'GET' && pathname === '/api/v1/checkout/') await chmod(token, 0o640);

      return undefined;
    }, attempts);
    await chmod(token, 0o600);

    try {
      const placed = await placement.call(later.client, approval);
      expect(placed.outcome, placement.name).toBe('accepted');
      expect(posts(later.sent), placement.name).toEqual(['POST ' + placement.path]);
      expect(await readAttempts(attempts)).toMatchObject([{ state: 'accepted' }]);
    } finally {
      await later.client.close();
    }
  }
});

test('other writes are sent once and report an unconfirmed change instead of inviting a retry', async () => {
  // 0 stands for a connection failure after the request was handed to fetch.
  let status = 0;

  const { client, sent } = scriptedClient((_pathname, method) => {
    if (method === 'GET') return undefined;

    return status === 0
      ? Promise.reject(new TypeError('socket hang up'))
      : new Response(status === 404 ? null : TOKEN, { status });
  });

  const noteAndBasketWrites = [
    () => client.addShoppingNoteLines({ lines: [{ text: 'Eggs' }] }),
    () => client.clearShoppingNote({ confirm: true }),
    () => client.setCheckoutLines({ lines: [{ sku: 'SKU-1' }], replace: false }),
  ];

  // Placed-order changes move money; after sending, no status proves they were not applied.
  const orderChanges = [
    () => client.deleteOrderLines({ orderToken: ORDER_TOKEN, lineIds: [1], confirm: true }),
    () =>
      client.lowerOrderLineQuantities({
        orderToken: ORDER_TOKEN,
        lineIds: [1],
        quantity: 0,
        confirm: true,
      }),
    () => client.toggleOrderLineSubstitution({ orderToken: ORDER_TOKEN, lineIds: [1] }),
  ];

  const expectations = [
    ...[
      [0, /did not confirm this change.*may have been applied/],
      [500, /did not confirm this change/],
      [400, /refused the request; nothing was changed/],
    ].map(([code, pattern]) => ({ code, pattern, writes: noteAndBasketWrites })),
    ...[0, 400, 404, 409, 422, 429, 500].map((code) => ({
      code,
      pattern: /did not confirm this order change; it may have been applied\. Read get_order/,
      writes: orderChanges,
    })),
  ];

  try {
    for (const { code, pattern, writes } of expectations) {
      status = Number(code);

      for (const write of writes) {
        const before = sent.length;
        const messages: string[] = [];
        assert(pattern instanceof RegExp);
        await captureFailure(messages, write, pattern);
        expect(messages[0]).not.toMatch(/Check the connection and try again/);
        expect(messages[0]).not.toContain(TOKEN);
        expect(sent.slice(before)).toHaveLength(1);
      }
    }
  } finally {
    await client.close();
  }
});

/** A settled money call as its outcome, or the fixed first clause of its refusal. */
function describeSettled(result: PromiseSettledResult<{ outcome: string }>): string {
  if (result.status === 'fulfilled') return result.value.outcome;

  const message = result.reason instanceof Error ? result.reason.message : '';

  return 'rejected: ' + message.split(/[.(]/)[0]?.trim();
}

test('concurrent calls for one approval send exactly one charge-bearing request', async () => {
  const release = Promise.withResolvers<void>();
  const postStarted = Promise.withResolvers<void>();

  const { client, sent } = scriptedClient(async (_pathname, method) => {
    if (method !== 'POST') return undefined;
    postStarted.resolve();
    await release.promise;

    return undefined;
  });

  const call = { ...approval, slotId: 501, addressId: 11, returnBags: false };

  try {
    // Same tool: the second call waits for the lock, then finds the accepted attempt.
    const first = client.completeCheckout(call);
    const second = client.completeCheckout(call);
    await postStarted.promise;
    await Bun.sleep(100);
    release.resolve();
    const settled = await Promise.allSettled([first, second]);

    // Either call may win the lock; exactly one is sent and the other finds its accepted record.
    expect(settled.map(describeSettled).toSorted()).toEqual([
      'accepted',
      'rejected: Krónan already accepted this order call for this exact checkout',
    ]);
    expect(posts(sent)).toEqual(['POST /checkout/complete/']);
  } finally {
    await client.close();
  }

  // Across tools, with the first outcome unknown: the waiting call is blocked, not sent.
  const crossRelease = Promise.withResolvers<void>();
  const crossStarted = Promise.withResolvers<void>();

  const cross = scriptedClient(async (_pathname, method) => {
    if (method !== 'POST') return undefined;
    crossStarted.resolve();
    await crossRelease.promise;

    throw new TypeError('socket hang up');
  });

  try {
    const first = cross.client.completeCheckout(call);
    const second = cross.client.reservePickupSlot({ ...approval, slotId: 601, returnBags: false });
    await crossStarted.promise;
    await Bun.sleep(100);
    crossRelease.resolve();
    const settled = await Promise.allSettled([first, second]);

    expect(settled.map(describeSettled).toSorted()).toEqual([
      'rejected: An earlier order call for this checkout is still unresolved',
      'unknown',
    ]);
    expect(posts(cross.sent)).toHaveLength(1);
  } finally {
    await cross.client.close();
  }
});

test('an unknown or submitting attempt blocks every money tool for that checkout', async () => {
  const { client, sent, attempts } = scriptedClient((_pathname, method) =>
    method === 'POST' ? Promise.reject(new TypeError('socket hang up')) : undefined,
  );

  try {
    const first = await client.completeCheckout({ ...approval, slotId: 501, returnBags: false });
    expect(first.outcome).toBe('unknown');
    expect(posts(sent)).toHaveLength(1);

    const recorded = await readAttempts(attempts);
    expect(recorded).toMatchObject([
      { tool: 'complete_checkout', state: 'unknown', checkoutToken: CHECKOUT_TOKEN },
    ]);

    if (process.platform !== 'win32') expect((await stat(attempts)).mode & 0o777).toBe(0o600);

    for (const placement of placements) {
      const before = sent.length;
      const messages: string[] = [];
      await captureFailure(
        messages,
        () => placement.call(client, approval),
        /still unresolved.*not permission to retry/,
      );
      expect(messages[0]).toMatch(NOTHING_SENT);
      // Blocked before the gate read: nothing at all reaches Krónan.
      expect(sent.slice(before), placement.name).toEqual([]);
    }

    // A crash between saving intent and the response leaves `submitting`; it blocks the same way.
    const [unknown] = recorded;
    assert(unknown);
    await writePrivateFile(
      attempts,
      JSON.stringify({
        version: 1,
        attempts: [{ ...unknown, tool: 'reserve_pickup_slot', state: 'submitting' }],
      }),
    );
    const before = sent.length;
    await assert.rejects(
      client.reserveDeliverySlot({ ...approval, slotId: 501, addressId: 11, returnBags: false }),
      /still unresolved/,
    );
    expect(sent.slice(before)).toEqual([]);

    // An unreadable record fails closed.
    await writePrivateFile(attempts, '{"version":1,"attempts":[{"state":"accepted"}]}');
    await assert.rejects(
      client.completeCheckout({ ...approval, slotId: 501, returnBags: false }),
      /record is unreadable or unsafe\. Nothing was sent/,
    );
    expect(sent.slice(before)).toEqual([]);
  } finally {
    await client.close();
  }
});

test('an accepted attempt blocks a repeat but still allows complete after reserve', async () => {
  const { client, sent } = scriptedClient(() => undefined);
  const complete = { ...approval, slotId: 501, addressId: 11, returnBags: false };

  try {
    const reserved = await client.reserveDeliverySlot(complete);
    expect(reserved.outcome).toBe('accepted');

    // The live reserve/complete sequence is unverified, so an accepted reserve must not block it.
    const completed = await client.completeCheckout(complete);
    expect(completed.outcome).toBe('accepted');
    expect(posts(sent)).toEqual(['POST /slots/delivery/reserve/', 'POST /checkout/complete/']);

    for (const repeat of [
      () => client.completeCheckout(complete),
      () => client.reserveDeliverySlot(complete),
      () => client.reservePickupSlot({ ...approval, slotId: 601, returnBags: false }),
      () => client.addCheckoutToOrder({ ...approval, expectedOrderToken: ORDER_TOKEN }),
    ]) {
      const before = sent.length;
      const messages: string[] = [];
      await captureFailure(messages, repeat, /already accepted this order call/);
      expect(messages[0]).toMatch(NOTHING_SENT);
      expect(posts(sent.slice(before))).toEqual([]);
    }
  } finally {
    await client.close();
  }

  // A changed checkout (different lines and total) is a new approval and may be sent.
  let current = checkout;

  const changed = scriptedClient((pathname) =>
    pathname === '/api/v1/checkout/' ? Response.json(current) : undefined,
  );

  try {
    expect((await changed.client.completeCheckout(complete)).outcome).toBe('accepted');
    const [line] = checkout.lines;
    assert(line);
    current = { ...checkout, lines: [{ ...line, quantity: 2 }], total: 1988 };
    expect(
      (await changed.client.completeCheckout({ ...complete, expectedTotal: 1988 })).outcome,
    ).toBe('accepted');
    expect(posts(changed.sent)).toHaveLength(2);
  } finally {
    await changed.client.close();
  }
});

// Rust: the cases that inject a lock into claimAttempt are unit tests in src/attempts.rs.
const RACED = /ran at the same time.*Nothing was sent to Krónan\. Read get_active_order/;

test('a record written by another holder during the gate stops the real client before its POST', async () => {
  let attemptsFile = '';

  // Another holder records an unresolved attempt for a different checkout while this gate reads.
  const { client, sent, attempts } = scriptedClient(async (pathname) => {
    if (pathname !== '/api/v1/checkout/') return undefined;
    await writePrivateFile(
      attemptsFile,
      JSON.stringify({
        version: 1,
        attempts: [
          {
            id: 'other-holder',
            tool: 'reserve_pickup_slot',
            checkoutToken: LIST_TOKEN,
            fingerprint: 'other-lines',
            total: 1,
            state: 'unknown',
            orderToken: null,
            createdAt: '2026-09-25T00:00:00.000Z',
            updatedAt: '2026-09-25T00:00:00.000Z',
          },
        ],
      }),
    );

    return undefined;
  });

  attemptsFile = attempts;

  try {
    await assert.rejects(
      client.completeCheckout({ ...approval, slotId: 501, returnBags: false }),
      RACED,
    );
    expect(sent).toEqual(['GET /checkout/']);
    expect(await readAttempts(attempts)).toMatchObject([{ id: 'other-holder', state: 'unknown' }]);
  } finally {
    await client.close();
  }
});

// Rust: the binary validates, so each check is awaited.
test('write inputs reject ambiguous or implicit requests', async () => {
  // Krónan replaces the whole checkout when replace is omitted; the caller must choose.
  expect((await setCheckoutLinesInput.safeParse({ lines: [{ sku: 'SKU-1' }] })).success).toBe(false);
  // Krónan deletes a note line when a change carries neither text nor quantity.
  expect((await changeShoppingNoteLineInput.safeParse({ token: NOTE_LINE_TOKEN })).success).toBe(false);
  expect(
    (await addShoppingNoteLinesInput.safeParse({ lines: [{ text: 'Eggs', sku: 'SKU-1' }] })).success,
  ).toBe(false);
  expect((await addShoppingNoteLinesInput.safeParse({ lines: [{ quantity: 1 }] })).success).toBe(false);
  expect((await addShoppingNoteLinesInput.safeParse({ lines: [] })).success).toBe(false);
  expect(
    (await addShoppingNoteLinesInput.safeParse({
      lines: Array.from({ length: 31 }, () => ({ text: 'Eggs' })),
    })).success,
  ).toBe(false);
  expect((await shoppingNoteLineTokenInput.safeParse({ token: 'not-a-token' })).success).toBe(false);
  expect((await clearShoppingNoteInput.safeParse({})).success).toBe(false);
  expect(
    (await deleteOrderLinesInput.safeParse({ orderToken: ORDER_TOKEN, lineIds: [1], confirm: false })).success,
  ).toBe(false);
  expect(
    (await lowerOrderLineQuantitiesInput.safeParse({ orderToken: ORDER_TOKEN, lineIds: [1], quantity: 1 })).success,
  ).toBe(false);
  expect(
    (await completeCheckoutInput.safeParse({
      ...approval,
      slotId: 1,
      returnBags: false,
      expectedTotal: -1,
    })).success,
  ).toBe(false);
  expect((await reservePickupSlotInput.safeParse({ ...approval, slotId: 1 })).success).toBe(false);
});

test('safe errors, strict inputs, and token non-disclosure', async () => {
  const errors: string[] = [];
  let response = new Response(TOKEN, { status: 401 });

  const client = new KronanClient(
    async () => TOKEN,
    async () => response,
  );

  try {
    await captureFailure(errors, () => client.status(), /Krónan rejected the access token/);
    response = new Response(TOKEN, { status: 403 });
    await captureFailure(errors, () => client.status(), /Krónan denied this request/);
    response = new Response(TOKEN, { status: 429 });
    await captureFailure(errors, () => client.status(), /Krónan rate limit reached/);

    const notFound = [
      () => client.product({ sku: 'SKU-1' }),
      () => client.order({ token: ORDER_TOKEN }),
      () => client.categoryProducts({ slug: 'dairy' }),
      () => client.productsByTag({ slug: 'vegan' }),
      () => client.productList({ token: LIST_TOKEN }),
      () => client.recipe({ slug: 'oat-cakes' }),
    ];

    const notFoundMessages = [
      /Product not found/,
      /Order not found/,
      /Category not found/,
      /Tag not found/,
      /Product list not found/,
      /Recipe not found/,
    ];

    for (const [index, run] of notFound.entries()) {
      response = new Response(TOKEN, { status: 404 });
      const pattern = notFoundMessages[index];
      assert(pattern);
      await captureFailure(errors, run, pattern);
    }

    response = new Response(null, { status: 404 });
    expect(await client.activeOrder()).toEqual({ active: false, order: null });

    response = new Response(TOKEN, { status: 500 });
    await captureFailure(errors, () => client.status(), /Krónan returned an error/);
    response = new Response(TOKEN, { status: 200 });
    await captureFailure(errors, () => client.status(), /Krónan returned an invalid API response/);
    response = Response.json({ type: 'user', upstreamOnly: TOKEN });
    await captureFailure(errors, () => client.status(), /outside the documented schema/);

    response = Response.json({ type: 'user', name: 'Account' });
    await captureFailure(errors, () => client.product({}), /Provide exactly one of sku or barcode/);
    await captureFailure(
      errors,
      () => client.product({ sku: 'SKU-1', barcode: '12345678' }),
      /Provide exactly one of sku or barcode/,
    );
    await captureFailure(errors, () => client.orders({ year: 2025 }), /year and month together/);
    await captureFailure(
      errors,
      () => client.orderLineSummary({ fromYear: 2025, nameContains: 'milk' }),
      /fromYear, fromMonth, toYear, and toMonth together/,
    );
    await captureFailure(
      errors,
      () => client.orderLineSummary({ nameContains: 'milk', skus: ['SKU-1'] }),
      /exactly one of nameContains or skus/,
    );
    await captureFailure(
      errors,
      () => client.orderLineSummary({}),
      /exactly one of nameContains or skus/,
    );

    // Rust: the binary validates, so each result is awaited.
    const strictResults = await Promise.all([
      emptyInput.safeParse(withUnexpectedKey({})),
      searchProductsInput.safeParse(withUnexpectedKey({ query: 'milk' })),
      getProductInput.safeParse(withUnexpectedKey({ sku: 'SKU-1' })),
      lookupProductsInput.safeParse(withUnexpectedKey({ skus: ['SKU-1'] })),
      categoryProductsInput.safeParse(withUnexpectedKey({ slug: 'dairy' })),
      productsByTagInput.safeParse(withUnexpectedKey({ slug: 'vegan' })),
      pageInput.safeParse(withUnexpectedKey({})),
      listOrdersInput.safeParse(withUnexpectedKey({})),
      orderInput.safeParse(withUnexpectedKey({ token: ORDER_TOKEN })),
      summarizeOrderLinesInput.safeParse(withUnexpectedKey({ nameContains: 'milk' })),
      purchaseStatsInput.safeParse(withUnexpectedKey({})),
      offsetInput.safeParse(withUnexpectedKey({})),
      productListInput.safeParse(withUnexpectedKey({ token: LIST_TOKEN })),
      searchRecipesInput.safeParse(withUnexpectedKey({})),
      recipeInput.safeParse(withUnexpectedKey({ slug: 'oat-cakes' })),
      addShoppingNoteLinesInput.safeParse(withUnexpectedKey({ lines: [{ text: 'Eggs' }] })),
      changeShoppingNoteLineInput.safeParse(
        withUnexpectedKey({ token: NOTE_LINE_TOKEN, text: 'Eggs' }),
      ),
      shoppingNoteLineTokenInput.safeParse(withUnexpectedKey({ token: NOTE_LINE_TOKEN })),
      clearShoppingNoteInput.safeParse(withUnexpectedKey({ confirm: true })),
      previewCheckoutLinesInput.safeParse(withUnexpectedKey({ lines: [{ sku: 'SKU-1' }] })),
      setCheckoutLinesInput.safeParse(
        withUnexpectedKey({ lines: [{ sku: 'SKU-1' }], replace: false }),
      ),
      reserveDeliverySlotInput.safeParse(
        withUnexpectedKey({ ...approval, slotId: 1, addressId: 1, returnBags: false }),
      ),
      reservePickupSlotInput.safeParse(
        withUnexpectedKey({ ...approval, slotId: 1, returnBags: false }),
      ),
      completeCheckoutInput.safeParse(
        withUnexpectedKey({ ...approval, slotId: 1, returnBags: false }),
      ),
      addCheckoutToOrderInput.safeParse(
        withUnexpectedKey({ ...approval, expectedOrderToken: ORDER_TOKEN }),
      ),
      deleteOrderLinesInput.safeParse(
        withUnexpectedKey({ orderToken: ORDER_TOKEN, lineIds: [1], confirm: true }),
      ),
      lowerOrderLineQuantitiesInput.safeParse(
        withUnexpectedKey({ orderToken: ORDER_TOKEN, lineIds: [1], quantity: 0, confirm: true }),
      ),
      toggleOrderLineSubstitutionInput.safeParse(
        withUnexpectedKey({ orderToken: ORDER_TOKEN, lineIds: [1] }),
      ),
    ]);

    for (const result of strictResults) {
      if (result.success) assert.fail('An input accepted an unknown key.');
      expect(result.error.issues.some((issue) => issue.code === 'unrecognized_keys')).toBe(true);
      errors.push(result.error.message);
    }

    expect((await getProductInput.safeParse({})).success).toBe(false);
    expect((await getProductInput.safeParse({ sku: 'SKU-1', barcode: '12345678' })).success).toBe(false);
    expect((await listOrdersInput.safeParse({ year: 2025 })).success).toBe(false);
    expect(
      (await summarizeOrderLinesInput.safeParse({ fromYear: 2025, nameContains: 'milk' })).success,
    ).toBe(false);
    expect(
      (await summarizeOrderLinesInput.safeParse({ nameContains: 'milk', skus: ['SKU-1'] })).success,
    ).toBe(false);
    expect((await summarizeOrderLinesInput.safeParse({})).success).toBe(false);
    expect(errors.join('\n')).not.toMatch(new RegExp(TOKEN));
  } finally {
    await client.close();
  }
});

type Annotations = { readOnlyHint: boolean; destructiveHint: boolean; idempotentHint: boolean };

const READ: Annotations = { readOnlyHint: true, destructiveHint: false, idempotentHint: true };

/** get_checkout and get_shopping_note: Krónan creates an empty resource on first read. */
const IDEMPOTENT_WRITE: Annotations = { ...READ, readOnlyHint: false };

const REPEATABLE_WRITE: Annotations = { ...IDEMPOTENT_WRITE, idempotentHint: false };

const DESTRUCTIVE: Annotations = { ...REPEATABLE_WRITE, destructiveHint: true };

/** Tools absent here are READ. */
const WRITE_ANNOTATIONS = new Map<string, Annotations>([
  ['get_checkout', IDEMPOTENT_WRITE],
  ['get_shopping_note', IDEMPOTENT_WRITE],
  ['add_shopping_note_lines', REPEATABLE_WRITE],
  ['change_shopping_note_line', IDEMPOTENT_WRITE],
  ['toggle_shopping_note_line_complete', REPEATABLE_WRITE],
  ['delete_shopping_note_line', DESTRUCTIVE],
  ['clear_shopping_note', DESTRUCTIVE],
  ['set_checkout_lines', DESTRUCTIVE],
  ['reserve_delivery_slot', DESTRUCTIVE],
  ['reserve_pickup_slot', DESTRUCTIVE],
  ['complete_checkout', DESTRUCTIVE],
  ['add_checkout_to_order', DESTRUCTIVE],
  ['delete_order_lines', DESTRUCTIVE],
  ['lower_order_line_quantities', DESTRUCTIVE],
  ['toggle_order_line_substitution', REPEATABLE_WRITE],
]);

/** Every call that can create an order or authorize or raise a charge requires the approval gate. */
const MONEY_TOOLS = [
  'reserve_delivery_slot',
  'reserve_pickup_slot',
  'complete_checkout',
  'add_checkout_to_order',
];

const toolArguments = {
  auth_status: {},
  search_products: { query: 'milk' },
  get_product: { sku: 'SKU-1' },
  lookup_products: { skus: ['SKU-1'] },
  list_categories: {},
  list_category_products: { slug: 'dairy' },
  list_product_tags: {},
  list_products_by_tag: { slug: 'vegan' },
  list_products_on_sale: {},
  list_favorite_products: {},
  list_orders: {},
  get_order: { token: ORDER_TOKEN },
  get_active_order: {},
  summarize_order_lines: { skus: ['SKU-1'] },
  list_purchase_stats: {},
  get_shopping_note: {},
  list_archived_shopping_note_lines: {},
  list_product_lists: {},
  get_product_list: { token: LIST_TOKEN },
  list_recipes: {},
  search_recipes: {},
  get_recipe: { slug: 'oat-cakes' },
  list_favorite_recipes: {},
  list_addresses: {},
  get_delivery_slots: { addressId: 11 },
  get_pickup_slots: {},
  get_checkout: {},
  add_shopping_note_lines: { lines: [{ text: 'Eggs' }] },
  change_shopping_note_line: { token: NOTE_LINE_TOKEN, text: 'Free-range eggs' },
  toggle_shopping_note_line_complete: { token: NOTE_LINE_TOKEN },
  delete_shopping_note_line: { token: NOTE_LINE_TOKEN },
  clear_shopping_note: { confirm: true },
  preview_checkout_lines: { lines: [{ sku: 'SKU-1', quantity: 2 }] },
  set_checkout_lines: { lines: [{ sku: 'SKU-1' }], replace: false },
  reserve_delivery_slot: { ...approval, slotId: 501, addressId: 11, returnBags: false },
  reserve_pickup_slot: { ...approval, slotId: 601, returnBags: false },
  complete_checkout: { ...approval, slotId: 501, addressId: 11, returnBags: false },
  add_checkout_to_order: { ...approval, expectedOrderToken: ORDER_TOKEN },
  delete_order_lines: { orderToken: ORDER_TOKEN, lineIds: [1], confirm: true },
  lower_order_line_quantities: {
    orderToken: ORDER_TOKEN,
    lineIds: [1],
    quantity: 0,
    confirm: true,
  },
  toggle_order_line_substitution: { orderToken: ORDER_TOKEN, lineIds: [1] },
};

test('all 41 MCP tools declare honest annotations and round-trip strict validated results', async () => {
  const attempts = scratchFile();

  const api = new KronanClient(
    async () => TOKEN,
    async (url) => fixtureResponse(new URL(url).pathname),
    attempts,
  );

  // Rust: the server is the binary, serving api's token, record and upstream over stdio.
  const server = await api.serve();
  const client = new Client({ name: 'kronan-roundtrip', version: VERSION });
  const clientTransport = server.transport;

  try {
    await client.connect(clientTransport);
    const listed = await client.listTools();
    const names = listed.tools.map((tool) => tool.name).toSorted();

    expect(names).toEqual(manifest.familyMcp.release.tools.toSorted());
    expect(names).toEqual(Object.keys(toolArguments).toSorted());
    expect(listed.tools.every((tool) => tool.outputSchema !== undefined)).toBe(true);

    for (const tool of listed.tools) {
      const { readOnlyHint, destructiveHint, idempotentHint } = tool.annotations ?? {};

      expect({ readOnlyHint, destructiveHint, idempotentHint }, tool.name).toEqual(
        WRITE_ANNOTATIONS.get(tool.name) ?? READ,
      );
    }

    for (const name of [
      ...MONEY_TOOLS,
      'clear_shopping_note',
      'delete_order_lines',
      'lower_order_line_quantities',
    ]) {
      const tool = listed.tools.find((entry) => entry.name === name);
      const properties = JSON.stringify(tool?.inputSchema);

      expect(tool?.inputSchema.required, name).toContain('confirm');
      expect(properties, name).toContain('"const":true');
    }

    for (const name of MONEY_TOOLS) {
      const tool = listed.tools.find((entry) => entry.name === name);

      for (const field of ['confirm', 'expectedTotal', 'expectedCheckoutToken'])
        expect(tool?.inputSchema.required, name).toContain(field);
    }

    const results = [];

    for (const [name, args] of Object.entries(toolArguments)) {
      // Every money call here is its own approval of the same fixture checkout.
      await rm(attempts, { force: true });
      const outcome = await client.callTool({ name, arguments: args });

      expect(outcome.isError, name).not.toBe(true);
      expect(outcome.structuredContent, name).toBeDefined();
      results.push(outcome);
    }

    const invalid = await client.callTool({
      name: 'search_products',
      arguments: { query: 'milk', unexpected: TOKEN },
    });

    expect(invalid.isError).toBe(true);
    expect(invalid.structuredContent).toBeUndefined();
    const transcript = JSON.stringify([listed, results, invalid]);
    expect(transcript).not.toContain(TOKEN);
    expect(transcript).not.toContain(UPSTREAM_EXTRA);
  } finally {
    await client.close();
    await server.close();
    await api.close();
  }
});

test('nested nutrition data is rejected and leading-punctuation slugs reach Krónan encoded', async () => {
  const NESTED_MARKER = 'nested-upstream-marker-7c1f';
  const requested: string[] = [];

  const api = new KronanClient(
    async () => TOKEN,
    async (url) => {
      const { pathname } = new URL(url);
      requested.push(pathname);

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
    },
  );

  try {
    const messages: string[] = [];
    await captureFailure(
      messages,
      () => api.product({ sku: 'SKU-1' }),
      /outside the documented schema/,
    );
    await captureFailure(
      messages,
      () => api.lookupProducts({ skus: ['SKU-1'] }),
      /outside the documented schema/,
    );
    expect(messages.join('\n')).not.toContain(NESTED_MARKER);

    const flat = new KronanClient(
      async () => TOKEN,
      async () =>
        Response.json({ ...product, nutrition: { energy: '100 kcal', fat: 2.5, salt: null } }),
    );

    expect((await flat.product({ sku: 'SKU-1' })).nutrition).toEqual({
      energy: '100 kcal',
      fat: 2.5,
      salt: null,
    });
    await flat.close();

    expect((await api.categoryProducts({ slug: '_dairy' })).name).toBe('Dairy');
    expect((await api.recipe({ slug: '-recipe' })).slug).toBe('oat-cakes');
    expect((await api.recipe({ slug: 'mjólk' })).slug).toBe('oat-cakes');
    expect(requested.slice(-3)).toEqual([
      '/api/v1/categories/_dairy/products/',
      '/api/v1/recipes/-recipe/',
      '/api/v1/recipes/mj%C3%B3lk/',
    ]);
    expect((await categoryProductsInput.safeParse({ slug: 'a/b' })).success).toBe(false);
    expect((await recipeInput.safeParse({ slug: '../x' })).success).toBe(false);

    // Bare dot segments would be collapsed by URL parsing into a different endpoint.
    for (const segment of ['.', '..']) {
      expect((await recipeInput.safeParse({ slug: segment })).success).toBe(false);
      expect((await categoryProductsInput.safeParse({ slug: segment })).success).toBe(false);
      expect((await getProductInput.safeParse({ sku: segment })).success).toBe(false);
      expect((await lookupProductsInput.safeParse({ skus: [segment] })).success).toBe(false);
    }

    expect((await getProductInput.safeParse({ sku: 'a.b' })).success).toBe(true);
  } finally {
    await api.close();
  }
});

test('stdio executable reports a missing token through MCP without stderr noise', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-stdio-'));
  const client = new Client({ name: 'kronan-stdio', version: VERSION });
  let stderr = '';

  // Rust: the stdio executable is the binary.
  const transport = new StdioClientTransport({
    ...serveCommand(),
    cwd: resolve('.'),
    env: {
      ...process.env,
      KRONAN_TOKEN_FILE: join(directory, 'missing.json'),
      XDG_CONFIG_HOME: join(directory, 'config'),
    },
    stderr: 'pipe',
  });

  transport.stderr?.on('data', (chunk) => {
    stderr += String(chunk);
  });

  try {
    await client.connect(transport);
    const result = await client.callTool({ name: 'auth_status', arguments: {} });
    expect(result.isError).toBe(true);
    expect(JSON.stringify(result)).toContain('No saved Krónan access token');
    await client.close();
    expect(stderr).toBe('');
  } finally {
    await client.close().catch(() => {});
    await rm(directory, { recursive: true, force: true });
  }
});

type CliOptions = {
  tokenFile: string;
  /** XDG_CONFIG_HOME; default `configFor(tokenFile)`. */
  config?: string;
  env?: Record<string, string>;
  preloadFile?: string;
  recordFile?: string;
  status?: string;
  input?: string;
};

async function runCli(args: string[], options: CliOptions) {
  // Rust: the binary, its upstream the preload file's fetch.
  const child = await spawnCli(options.preloadFile, args, {
    cwd: resolve('.'),
    env: {
      ...process.env,
      KRONAN_TOKEN_FILE: options.tokenFile,
      XDG_CONFIG_HOME: options.config ?? configFor(options.tokenFile),
      ...options.env,
      KRONAN_PRELOAD_RECORD: options.recordFile,
      KRONAN_PRELOAD_STATUS: options.status ?? '200',
    },
    stdin: options.input === undefined ? 'ignore' : 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  });

  if (options.input !== undefined) {
    await child.stdin?.write(options.input);
    await child.stdin?.end();
  }

  const [exitCode, stdout, stderr] = await Promise.all([
    child.exited,
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
  ]);

  return { exitCode, stdout, stderr };
}

test('CLI token setup, status, logout, help, version, and unknown commands stay offline', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-cli-'));
  const preload = join(directory, 'mock-fetch.js');
  const source = join(directory, 'token.txt');
  const saved = join(directory, 'saved.json');
  const fileRecord = join(directory, 'file-request.json');
  const stdinSaved = join(directory, 'stdin.json');
  const stdinRecord = join(directory, 'stdin-request.json');
  const rejected = join(directory, 'rejected.json');
  const rejectedRecord = join(directory, 'rejected-request.json');
  const invalid = join(directory, 'invalid.json');
  const invalidRecord = join(directory, 'invalid-request.json');

  const sourceText = [
    "import { writeFile } from 'node:fs/promises';",
    'globalThis.fetch = async (input, init) => {',
    '  const recordPath = process.env.KRONAN_PRELOAD_RECORD;',
    "  if (!recordPath) throw new Error('Missing local request record path.');",
    '  const request = {',
    '    url: String(input),',
    '    authorization: new Headers(init?.headers).get("authorization"),',
    '  };',
    '  await writeFile(recordPath, JSON.stringify(request), { mode: 0o600 });',
    "  if (process.env.KRONAN_PRELOAD_STATUS === '401')",
    "    return new Response('synthetic-token rejected', { status: 401 });",
    "  return Response.json({ type: 'user', name: 'Test' });",
    '};',
    '',
  ].join('\n');

  try {
    await writeFile(preload, sourceText, { mode: 0o600 });
    await writeFile(source, '  ' + TOKEN + ' \n', { mode: 0o600 });

    const fromFile = await runCli(['auth', 'set', source], {
      tokenFile: saved,
      preloadFile: preload,
      recordFile: fileRecord,
    });

    expect(fromFile.exitCode).toBe(0);
    expect(fromFile.stderr).toBe('');
    expect(fromFile.stdout).toContain('verified and saved encrypted');
    expect(await withTokenFiles(saved, () => loadToken())).toBe(TOKEN);
    await assert.rejects(stat(saved), { code: 'ENOENT' });
    expect(await readFile(recordIn(configFor(saved)), 'utf8')).not.toContain(TOKEN);

    if (process.platform !== 'win32')
      expect((await stat(recordIn(configFor(saved)))).mode & 0o777).toBe(0o600);
    expect(await readFile(fileRecord, 'utf8')).toBe(
      JSON.stringify({
        url: 'https://api.kronan.is/api/v1/me/',
        authorization: 'AccessToken ' + TOKEN,
      }),
    );

    const fromStdin = await runCli(['auth', 'set', '-'], {
      tokenFile: stdinSaved,
      preloadFile: preload,
      recordFile: stdinRecord,
      input: TOKEN + '\n',
    });

    expect(fromStdin.exitCode).toBe(0);
    expect(await withTokenFiles(stdinSaved, () => loadToken())).toBe(TOKEN);
    await assert.rejects(stat(stdinSaved), { code: 'ENOENT' });

    if (process.platform !== 'win32')
      expect((await stat(recordIn(configFor(stdinSaved)))).mode & 0o777).toBe(0o600);
    expect(await readFile(stdinRecord, 'utf8')).toBe(
      JSON.stringify({
        url: 'https://api.kronan.is/api/v1/me/',
        authorization: 'AccessToken ' + TOKEN,
      }),
    );

    const rejectedSet = await runCli(['auth', 'set', source], {
      tokenFile: rejected,
      preloadFile: preload,
      recordFile: rejectedRecord,
      status: '401',
    });

    expect(rejectedSet.exitCode).toBe(1);
    expect(rejectedSet.stderr).toMatch(/Krónan rejected the access token/);
    expect(rejectedSet.stderr).not.toContain(TOKEN);
    await assert.rejects(stat(recordIn(configFor(rejected))), { code: 'ENOENT' });

    const sharedSource = join(directory, 'shared-token.txt');
    await writeFile(sharedSource, TOKEN + '\n', { mode: 0o644 });
    // writeFile's mode is masked by umask; the fixture must be group-readable regardless.
    await chmod(sharedSource, 0o644);
    const linkedSource = join(directory, 'linked-token.txt');
    await symlink(source, linkedSource);

    for (const [unsafeSource, pattern] of [
      [sharedSource, /Cannot read the token source file/],
      [linkedSource, /Cannot read the token source file/],
      [join(directory, 'absent-token.txt'), /token source file does not exist/],
    ] as const) {
      const unsafeSaved = join(directory, 'unsafe.json');
      const unsafeRecord = join(directory, 'unsafe-request.json');

      const unsafeSet = await runCli(['auth', 'set', unsafeSource], {
        tokenFile: unsafeSaved,
        preloadFile: preload,
        recordFile: unsafeRecord,
      });

      // A group-readable or linked source is itself a leaked credential; nothing is sent or saved.
      expect(unsafeSet.exitCode).toBe(1);
      expect(unsafeSet.stderr).toMatch(pattern);
      expect(unsafeSet.stderr).not.toContain(TOKEN);
      await assert.rejects(stat(recordIn(configFor(unsafeSaved))), { code: 'ENOENT' });
      await assert.rejects(stat(unsafeRecord), { code: 'ENOENT' });
    }

    await writeFile(source, 'short\n', { mode: 0o600 });

    const invalidToken = await runCli(['auth', 'set', source], {
      tokenFile: invalid,
      preloadFile: preload,
      recordFile: invalidRecord,
    });

    expect(invalidToken.exitCode).toBe(1);
    expect(invalidToken.stderr).toMatch(/Invalid Krónan access token/);
    await assert.rejects(stat(recordIn(configFor(invalid))), { code: 'ENOENT' });
    await assert.rejects(stat(invalidRecord), { code: 'ENOENT' });

    const missingStatus = await runCli(['auth', 'status'], {
      tokenFile: join(directory, 'missing.json'),
    });

    expect(missingStatus.exitCode).toBe(1);
    expect(missingStatus.stderr).toMatch(/No saved Krónan access token/);

    await writeLegacyToken(saved, TOKEN);
    const logout = await runCli(['auth', 'logout'], { tokenFile: saved });
    expect(logout.exitCode).toBe(0);
    await assert.rejects(stat(saved), { code: 'ENOENT' });
    await assert.rejects(
      withTokenFiles(saved, () => loadToken()),
      /No saved Krónan access token/,
    );

    const version = await runCli(['--version'], { tokenFile: saved });
    expect(version.exitCode).toBe(0);
    expect(version.stdout).toBe(VERSION + '\n');
    const help = await runCli(['--help'], { tokenFile: saved });
    expect(help.exitCode).toBe(0);
    expect(help.stdout).toContain('stdio MCP server');
    const unknown = await runCli(['not-a-command'], { tokenFile: saved });
    expect(unknown.exitCode).toBe(1);
    expect(unknown.stderr).toContain('kronan-mcp');
    expect(
      [rejectedSet.stderr, invalidToken.stderr, missingStatus.stderr, unknown.stderr].join('\n'),
    ).not.toMatch(new RegExp(TOKEN));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('CLI migrate, status and set keep no plaintext token anywhere under the default paths', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-default-paths-'));
  const home = join(directory, 'home');
  const preload = join(directory, 'mock-fetch.js');
  const record = join(directory, 'request.json');
  const legacy = join(home, 'config', 'kronan-mcp', 'session.json');
  const next = 'synthetic-token-after-migration';

  const env = {
    HOME: home,
    XDG_CONFIG_HOME: join(home, 'config'),
    XDG_DATA_HOME: join(home, 'data'),
    KRONAN_TOKEN_FILE: '',
  };

  const run = (args: string[], input?: string) => {
    const options: CliOptions = { tokenFile: '', env, preloadFile: preload, recordFile: record };

    if (input !== undefined) options.input = input;

    return runCli(args, options);
  };

  /** Every file under the scratch home, so a plaintext copy anywhere fails the test. */
  const contents = async () => {
    const names = await readdir(home, { recursive: true });
    const files = [];

    for (const name of names)
      if ((await stat(join(home, name))).isFile())
        files.push(await readFile(join(home, name), 'utf8'));

    return files.join('\n');
  };

  try {
    await writeFile(
      preload,
      [
        "import { writeFile } from 'node:fs/promises';",
        'globalThis.fetch = async (input, init) => {',
        '  await writeFile(process.env.KRONAN_PRELOAD_RECORD, new Headers(init?.headers).get("authorization"));',
        "  return Response.json({ type: 'user', name: 'Test' });",
        '};',
        '',
      ].join('\n'),
      { mode: 0o600 },
    );
    await writeLegacyToken(legacy, TOKEN);

    const before = await run(['auth', 'status']);
    expect(before.exitCode).toBe(0);
    expect(before.stdout.split('\n')[0]).toBe(
      'Saved in a plaintext file. Run kronan-mcp auth migrate.',
    );
    expect(await readFile(record, 'utf8')).toBe('AccessToken ' + TOKEN);

    const migrated = await run(['auth', 'migrate']);
    expect(migrated.exitCode).toBe(0);
    expect(migrated.stdout).toContain('moved to the encrypted store');
    await assert.rejects(stat(legacy), { code: 'ENOENT' });
    expect(await contents()).not.toContain(TOKEN);
    await stat(join(home, 'config', 'kronan-mcp', 'session.enc'));
    expect(
      (await stat(join(home, 'data', 'family-mcp', 'keys', 'kronan-mcp.default.key'))).mode & 0o777,
    ).toBe(0o600);

    expect((await run(['auth', 'migrate'])).stdout).toBe('Already migrated.\n');

    const after = await run(['auth', 'status']);
    expect(after.exitCode).toBe(0);
    expect(after.stdout.split('\n')[0]).toBe('Saved in an encrypted file.');
    expect(after.stdout + after.stderr).not.toContain(home);

    const set = await run(['auth', 'set', '-'], next + '\n');
    expect(set.exitCode).toBe(0);
    expect(await readFile(record, 'utf8')).toBe('AccessToken ' + next);
    const stored = await contents();
    expect(stored).not.toContain(TOKEN);
    expect(stored).not.toContain(next);

    const logout = await run(['auth', 'logout']);
    expect(logout.exitCode).toBe(0);
    const status = await run(['auth', 'status']);
    expect(status.exitCode).toBe(1);
    expect(status.stderr).toMatch(/No saved Krónan access token/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('orders clear-attempts lists the record and clears it only after an explicit yes', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-clear-'));
  const tokenFile = join(directory, 'session.json');
  const attempts = tokenFile + '.order-attempts.json';

  const api = new KronanClient(
    async () => TOKEN,
    async (url, options) => {
      if (options.method === 'POST') throw new TypeError('socket hang up');

      return fixtureResponse(new URL(url).pathname);
    },
    attempts,
  );

  try {
    const empty = await runCli(['orders', 'clear-attempts'], { tokenFile });
    expect(empty.exitCode).toBe(0);
    expect(empty.stdout).toContain('No recorded order attempts');

    expect(
      (await api.completeCheckout({ ...approval, slotId: 501, returnBags: false })).outcome,
    ).toBe('unknown');

    for (const input of ['n\n', '\n', '', 'yes please\n']) {
      const kept = await runCli(['orders', 'clear-attempts'], { tokenFile, input });
      expect(kept.exitCode, JSON.stringify(input)).toBe(0);
      expect(kept.stdout).toContain('complete_checkout  unknown');
      expect(kept.stdout).toContain('Kept the recorded order attempts.');
      expect(kept.stderr).toContain('[y/N]');
      expect(await readAttempts(attempts)).toHaveLength(1);
    }

    const cleared = await runCli(['orders', 'clear-attempts'], { tokenFile, input: 'y\n' });
    expect(cleared.exitCode).toBe(0);
    expect(cleared.stdout).toContain('Cleared the recorded order attempts.');
    await assert.rejects(stat(attempts), { code: 'ENOENT' });
    expect(cleared.stdout + cleared.stderr).not.toContain(TOKEN);

    // An unreadable record can be inspected and cleared the same way.
    await writePrivateFile(attempts, 'not json');
    const invalid = await runCli(['orders', 'clear-attempts'], { tokenFile, input: 'Y\n' });
    expect(invalid.stdout).toContain('unreadable or unsafe');
    await assert.rejects(stat(attempts), { code: 'ENOENT' });
  } finally {
    await api.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test('SIGTERM aborts a pending API request and closes the stdio process', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'kronan-shutdown-'));
  const tokenFile = join(directory, 'token.json');
  const preload = join(directory, 'never-resolves.js');
  const fetchStarted = Promise.withResolvers<void>();
  const fetchAborted = Promise.withResolvers<void>();
  const stopped = Promise.withResolvers<void>();
  const client = new Client({ name: 'kronan-shutdown', version: VERSION });

  const source = [
    'globalThis.fetch = (_input, init) => new Promise((_resolve, reject) => {',
    "  process.stderr.write('FETCH_STARTED\\n');",
    '  const signal = init?.signal;',
    '  const abort = () => {',
    "    process.stderr.write('FETCH_ABORTED\\n');",
    "    reject(new DOMException('Aborted', 'AbortError'));",
    '  };',
    '  if (signal?.aborted) abort();',
    "  else signal?.addEventListener('abort', abort, { once: true });",
    '});',
    '',
  ].join('\n');

  await withTokenFiles(tokenFile, () => saveToken(TOKEN));
  await writeFile(preload, source, { mode: 0o600 });

  // Rust: the preload's fetch answers the binary's requests, so what it writes reaches this
  // process instead of the server's stderr.
  const served = await preloadUpstream(preload);

  const transport = new StdioClientTransport({
    ...serveCommand(),
    cwd: resolve('.'),
    env: {
      ...process.env,
      KRONAN_TOKEN_FILE: tokenFile,
      XDG_CONFIG_HOME: configFor(tokenFile),
      KRONAN_TEST_ORIGIN: served.origin,
    },
    stderr: 'pipe',
  });

  onPreloadOutput((output) => {
    if (output.includes('FETCH_STARTED')) fetchStarted.resolve();

    if (output.includes('FETCH_ABORTED')) fetchAborted.resolve();
  });
  // oxlint-disable-next-line unicorn/prefer-add-event-listener -- StdioClientTransport exposes onclose as a callback property.
  transport.onclose = () => stopped.resolve();
  let pending: Promise<object> | undefined;

  try {
    await client.connect(transport);
    const pid = transport.pid;
    assert(pid);
    pending = client.callTool({ name: 'auth_status', arguments: {} });
    void pending.catch(() => {});
    await fetchStarted.promise;
    process.kill(pid, 'SIGTERM');
    await Promise.race([
      fetchAborted.promise,
      Bun.sleep(5000).then(() => assert.fail('The in-flight fetch was not aborted.')),
    ]);
    await Promise.race([
      stopped.promise,
      Bun.sleep(5000).then(() => assert.fail('The stdio process did not stop after SIGTERM.')),
    ]);
    await pending.catch(() => {});
    expect(transport.pid).toBeNull();
  } finally {
    if (transport.pid !== null) process.kill(transport.pid, 'SIGKILL');
    await pending?.catch(() => {});
    await client.close().catch(() => {});
    await served.close();
    await rm(directory, { recursive: true, force: true });
  }
});
