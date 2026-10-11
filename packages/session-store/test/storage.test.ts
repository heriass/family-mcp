import { expect, test } from 'bun:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import {
  chmod,
  link,
  lstat,
  mkdir,
  mkdtemp,
  readdir,
  readFile,
  realpath,
  rm,
  stat,
  symlink,
  writeFile,
} from 'node:fs/promises';
import type { Stats } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  LocalKeyFileProvider,
  SessionStoreError,
  StoreRefusal,
  TEST_LS,
  checkSecretStore,
  createSecretKey,
  withSecretRecord,
  withSecretStore,
  type SecretRecordOptions,
} from '../src/index.js';
import { keyTemporary } from '../src/keys.js';
import {
  aclEntries,
  aclProblem,
  ancestorProblem,
  checkStorePaths,
  ownedDirectoryProblem,
  type StoreStat,
} from '../src/storage.js';

const acl = (operation: '+a' | '-a', entry: string, path: string) =>
  execFileSync('/bin/chmod', [operation, entry, path]);

const worker = fileURLToPath(new URL('./key-worker.ts', import.meta.url));

const UID = process.getuid?.() ?? 0;

const OTHER = UID + 4242;

const E1 =
  'This store folder is a link to another place. Replace it with a real folder and start again.';

const E2 = 'This store folder belongs to another user, often root after a sudo run.';

const E3 = 'Other users can open this store folder.';

const E4 = 'Other users can write to a folder above the store.';

const E5 = 'A folder above the store belongs to another user.';

const HARD_LINKS =
  'This file has a second name (a hard link). Remove the other name and start again.';

const OWNED_ACL =
  'Extra sharing permissions (an access control list, set in Finder’s Get Info) let other users in.';

const ANCESTOR_ACL =
  'Extra sharing permissions (an access control list) on a folder above the store let other users change it. List them with ls -led and remove the entry that allows another user to write.';

const TEMPORARY = '0f0e0d0c-0b0a-4908-8706-050403020100';

const refused =
  (message: string, path?: string) =>
  (cause: unknown): boolean =>
    cause instanceof StoreRefusal &&
    cause.code === 'UNSAFE_FILE' &&
    cause.message === message &&
    (path === undefined || cause.path === path);

function fake(kind: 'directory' | 'file' | 'link', mode: number, uid = UID, nlink = 1): StoreStat {
  return {
    mode,
    uid,
    nlink,
    isDirectory: () => kind === 'directory',
    isFile: () => kind === 'file',
    isSymbolicLink: () => kind === 'link',
  };
}

async function scratch(work: (directory: string) => Promise<void>): Promise<void> {
  const directory = await mkdtemp(join(tmpdir(), 'session-store-storage-'));

  try {
    await work(directory);
  } finally {
    // A test's deny-delete ACL would keep the scratch tree; ACLs go first on macOS.
    if (process.platform === 'darwin') execFileSync('/bin/chmod', ['-R', '-N', directory]);
    await rm(directory, { recursive: true, force: true });
  }
}

/** A key file provider that counts reads, so a test can show the preflight never reads it. */
class CountingKeys extends LocalKeyFileProvider {
  reads = 0;

  override getKey(): Promise<Uint8Array> {
    this.reads++;

    return super.getKey();
  }
}

/** The macOS layout under `root`: `family-mcp/{keys,test-mcp}`. */
function layout(root: string): SecretRecordOptions & { keys: CountingKeys; key: string } {
  const key = join(root, 'family-mcp', 'keys', 'test-mcp.default.key');

  return {
    path: join(root, 'family-mcp', 'test-mcp', 'session.enc'),
    server: 'test-mcp',
    profile: 'default',
    purpose: 'session',
    schema: 1,
    keys: new CountingKeys({ path: key }),
    key,
    maxBytes: 1024,
  };
}

async function listTree(directory: string): Promise<string[]> {
  return (await readdir(directory, { recursive: true })).toSorted();
}

test('store directory and ancestor decisions are pure', () => {
  expect(ownedDirectoryProblem(fake('directory', 0o40700), UID)).toBeUndefined();
  expect(ownedDirectoryProblem(fake('link', 0o120777), UID)).toBe(E1);
  expect(ownedDirectoryProblem(fake('directory', 0o40700, OTHER), UID)).toBe(E2);
  // A root-owned store directory is refused like any other owner.
  expect(ownedDirectoryProblem(fake('directory', 0o40700, 0), UID === 0 ? 1 : UID)).toBe(E2);
  expect(ownedDirectoryProblem(fake('directory', 0o40750), UID)).toBe(E3);
  expect(ownedDirectoryProblem(fake('directory', 0o40701), UID)).toBe(E3);
  expect(ownedDirectoryProblem(fake('file', 0o100600), UID)).toBe(
    'Something other than a folder is at this store path. Move it away and start again.',
  );

  expect(ancestorProblem(fake('directory', 0o40755), UID, UID)).toBeUndefined();
  expect(ancestorProblem(fake('directory', 0o40755, 0), UID, UID)).toBeUndefined();
  expect(ancestorProblem(fake('directory', 0o40750), UID, UID)).toBeUndefined();
  expect(ancestorProblem(fake('directory', 0o40775), UID, UID)).toBe(E4);
  expect(ancestorProblem(fake('directory', 0o40757), UID, UID)).toBe(E4);
  expect(ancestorProblem(fake('directory', 0o40775, 0), UID, UID)).toBe(E4);
  expect(ancestorProblem(fake('directory', 0o40755, OTHER), UID, UID)).toBe(E5);
  // A sticky shared directory such as /tmp protects an entry only its owner or root can move.
  expect(ancestorProblem(fake('directory', 0o41777, 0), UID, UID)).toBeUndefined();
  expect(ancestorProblem(fake('directory', 0o41777, 0), UID, 0)).toBeUndefined();
  expect(ancestorProblem(fake('directory', 0o41777, 0), UID, OTHER)).toBe(E4);
  expect(ancestorProblem(fake('directory', 0o41777, 0), UID, undefined)).toBe(E4);
});

test('ACL entries: deny passes, any grant on the store and a changing grant above it fail', () => {
  const listing = [
    'drwxr-x---+ 152 me  staff  4864 Oct  9 14:47 /Users/me',
    ' 0: group:everyone deny delete',
    'drwx------  3 me  staff  96 Oct  9 14:47 /Users/me/Library/Application Support/family-mcp',
    '-rw-------+ 1 me  staff  32 Oct  9 14:47 /Users/me/key',
    ' 0: user:other allow read',
    ' 1: group:everyone inherited deny delete',
    '',
  ].join('\n');

  const [home, root, key] = aclEntries(listing);

  expect(aclEntries(listing)).toHaveLength(3);
  expect(home).toEqual([' 0: group:everyone deny delete']);
  expect(root).toEqual([]);
  expect(key).toHaveLength(2);

  for (const role of ['owned', 'ancestor'] as const) {
    expect(aclProblem(home ?? [], role, 'me')).toBeUndefined();
    expect(aclProblem([], role, 'me')).toBeUndefined();
    expect(aclProblem([' 0: something unexpected'], role, 'me')).toBe(
      role === 'owned' ? OWNED_ACL : ANCESTOR_ACL,
    );
  }

  expect(aclProblem(key ?? [], 'owned', 'me')).toBe(OWNED_ACL);
  expect(aclProblem([' 0: user:me allow read'], 'owned', 'me')).toBe(OWNED_ACL);
  expect(aclProblem(key ?? [], 'ancestor', 'me')).toBeUndefined();
  expect(aclProblem([' 0: user:me allow add_file,delete_child'], 'ancestor', 'me')).toBeUndefined();

  for (const permission of ['add_file', 'delete_child', 'add_subdirectory', 'writesecurity'])
    expect(
      aclProblem([` 0: group:staff inherited allow list,${permission}`], 'ancestor', 'me'),
    ).toBe(ANCESTOR_ACL);
});

test('the preflight passes on a missing store, creates nothing and reads no key', async () => {
  await scratch(async (root) => {
    const store = layout(root);
    expect(await checkSecretStore(store)).toEqual({ exists: false, retired: [] });
    expect(await listTree(root)).toEqual([]);
    expect(store.keys.reads).toBe(0);

    // Set up, then the preflight still reads nothing.
    await createSecretKey(store);
    await withSecretRecord(store, async () => 'secret');
    const reads = store.keys.reads;
    const before = await listTree(root);
    expect(await checkSecretStore(store)).toEqual({ exists: true, retired: [] });
    expect(store.keys.reads).toBe(reads);
    expect(await listTree(root)).toEqual(before);

    for (const directory of ['family-mcp', 'family-mcp/keys', 'family-mcp/test-mcp'])
      expect((await stat(join(root, directory))).mode & 0o777).toBe(0o700);

    for (const file of [store.key, store.path, `${store.path}.marker`])
      expect((await stat(file)).mode & 0o777).toBe(0o600);
  });
});

test('the preflight refuses unsafe files without reading the key', async () => {
  await scratch(async (root) => {
    const store = layout(root);
    await createSecretKey(store);
    await withSecretRecord(store, async () => 'secret');
    const reads = store.keys.reads;
    const marker = `${store.path}.marker`;

    for (const file of [store.key, store.path, marker]) {
      await chmod(file, 0o644);
      await assert.rejects(
        checkSecretStore(store),
        refused('Other users can open this file.', file),
      );
      await chmod(file, 0o600);

      const alias = join(root, 'alias');
      await link(file, alias);
      await assert.rejects(checkSecretStore(store), refused(HARD_LINKS, file));
      await rm(alias);
    }

    const saved = await readFile(store.key);
    await rm(store.key);
    await writeFile(join(root, 'elsewhere.key'), saved, { mode: 0o600 });
    await symlink(join(root, 'elsewhere.key'), store.key);
    await assert.rejects(
      checkSecretStore(store),
      refused(
        'This file is a link to another file. Put the real file here and start again.',
        store.key,
      ),
    );
    expect(store.keys.reads).toBe(reads);
  });
});

test('a store directory with group or other bits is refused and left unchanged', async () => {
  await scratch(async (root) => {
    const store = layout(root);
    const records = join(root, 'family-mcp', 'test-mcp');
    const keys = join(root, 'family-mcp', 'keys');
    await mkdir(records, { recursive: true, mode: 0o700 });
    await chmod(records, 0o750);

    await assert.rejects(checkSecretStore(store), refused(E3, records));
    await assert.rejects(
      withSecretStore(store, async () => assert.fail()),
      refused(E3, records),
    );
    expect((await stat(records)).mode & 0o777).toBe(0o750);
    expect(await readdir(records)).toEqual([]);

    await chmod(records, 0o700);
    await mkdir(keys, { mode: 0o750 });
    await chmod(keys, 0o750);
    await assert.rejects(store.keys.getKey(), refused(E3, keys));
    await assert.rejects(store.keys.createKey(), refused(E3, keys));
    await assert.rejects(createSecretKey(store), refused(E3, keys));
    expect(await readdir(keys)).toEqual([]);
    expect((await stat(keys)).mode & 0o777).toBe(0o750);

    // The store root is a store directory too.
    await chmod(keys, 0o700);
    await chmod(join(root, 'family-mcp'), 0o755);
    await assert.rejects(checkSecretStore(store), refused(E3, join(root, 'family-mcp')));
  });
});

test('a symlinked store directory is refused', async () => {
  await scratch(async (root) => {
    const store = layout(root);
    await mkdir(join(root, 'family-mcp'), { mode: 0o700 });
    await mkdir(join(root, 'real-records'), { mode: 0o700 });
    await mkdir(join(root, 'real-keys'), { mode: 0o700 });
    await symlink(join(root, 'real-records'), join(root, 'family-mcp', 'test-mcp'));
    await symlink(join(root, 'real-keys'), join(root, 'family-mcp', 'keys'));

    await assert.rejects(
      withSecretStore(store, async () => assert.fail()),
      refused(E1, join(root, 'family-mcp', 'test-mcp')),
    );
    await assert.rejects(store.keys.getKey(), refused(E1, join(root, 'family-mcp', 'keys')));
    await assert.rejects(store.keys.createKey(), refused(E1, join(root, 'family-mcp', 'keys')));
    expect(await readdir(join(root, 'real-keys'))).toEqual([]);
    expect(await readdir(join(root, 'real-records'))).toEqual([]);
  });
});

test('ancestors that another user could replace are refused, also above a missing store', async () => {
  await scratch(async (root) => {
    const shared = join(root, 'shared');
    await mkdir(shared, { mode: 0o775 });
    await chmod(shared, 0o775);
    const store = layout(shared);

    await assert.rejects(checkSecretStore(store), refused(E4, shared));
    await assert.rejects(
      withSecretStore(store, async () => assert.fail()),
      refused(E4, shared),
    );
    expect(await readdir(shared)).toEqual([]);

    // A link above the store is followed: the directory it resolves to is checked too.
    await chmod(shared, 0o755);
    const open = join(root, 'open');
    await mkdir(join(open, 'inner'), { recursive: true, mode: 0o700 });
    await chmod(open, 0o777);
    await symlink(join(open, 'inner'), join(root, 'alias'));
    await assert.rejects(
      checkSecretStore(layout(join(root, 'alias'))),
      refused(E4, await realpath(open)),
    );

    // Sticky and shared like /tmp: only this user can move the entry below it.
    await chmod(open, 0o1777);

    // A refusal was seen once in CI (job 114213135837) and never reproduced: name what the sticky
    // exemption saw, so the next failure carries its cause.
    try {
      expect(await checkSecretStore(layout(join(root, 'alias')))).toEqual({
        exists: false,
        retired: [],
      });
    } catch (error) {
      const [outer, inner] = await Promise.all([lstat(open), lstat(join(open, 'inner'))]);

      assert.fail(
        `${String(error)}: open mode ${outer.mode.toString(8)} uid ${outer.uid}, ` +
          `open/inner uid ${inner.uid}, process uid ${process.getuid?.()}, ` +
          `umask ${process.umask().toString(8)}`,
      );
    }
  });
});

test('every link above the store is followed, also links inside a link’s target', async () => {
  // The written route and the final target are safe; the hop between them is replaceable.
  const outside = await mkdtemp(join(tmpdir(), 'session-store-hop-'));

  try {
    await scratch(async (root) => {
      const open = join(outside, 'open');
      await mkdir(open, { mode: 0o700 });
      await chmod(open, 0o777);
      await mkdir(join(root, 'safe'), { mode: 0o700 });
      await symlink(join(root, 'safe'), join(open, 'hop'));
      await symlink(join(open, 'hop'), join(root, 'alias'));
      const store = layout(join(root, 'alias'));

      await assert.rejects(checkSecretStore(store), refused(E4, await realpath(open)));
      await assert.rejects(
        withSecretStore(store, async () => assert.fail()),
        refused(E4, await realpath(open)),
      );
      await assert.rejects(store.keys.createKey(), refused(E4, await realpath(open)));
      expect(await readdir(join(root, 'safe'))).toEqual([]);

      // The same hop through a relative link.
      await symlink('safe', join(root, 'relative'));
      await rm(join(open, 'hop'));
      await symlink(join(root, 'relative'), join(open, 'hop'));
      await assert.rejects(checkSecretStore(store), refused(E4, await realpath(open)));

      // Once the hop's directory is safe, the route is.
      await chmod(open, 0o755);
      expect(await checkSecretStore(store)).toEqual({ exists: false, retired: [] });
      await createSecretKey(store);
      expect(await readdir(join(root, 'safe'))).toEqual(['family-mcp']);

      // A loop of links is refused, not followed forever.
      await symlink(join(root, 'loop-b'), join(root, 'loop-a'));
      await symlink(join(root, 'loop-a'), join(root, 'loop-b'));
      await assert.rejects(
        checkSecretStore(layout(join(root, 'loop-a'))),
        refused(
          'A link in a folder above the store is broken, loops back on itself, or leads into a folder you cannot open.',
        ),
      );
      // chmod -R cannot pass a loop on cleanup.
      await rm(join(root, 'loop-a'));
      await rm(join(root, 'loop-b'));
    });
  } finally {
    await rm(outside, { recursive: true, force: true });
  }
});

test.skipIf(process.platform !== 'darwin')(
  'macOS ACLs that grant other users access are refused; deny entries pass',
  async () => {
    await scratch(async (root) => {
      const store = layout(root);
      await createSecretKey(store);
      await withSecretRecord(store, async () => 'secret');
      const records = join(root, 'family-mcp', 'test-mcp');

      acl('+a', 'everyone deny delete', records);
      expect((await checkSecretStore(store)).exists).toBe(true);

      for (const path of [store.key, store.path, records]) {
        acl('+a', 'everyone allow read', path);
        await assert.rejects(checkSecretStore(store), refused(OWNED_ACL, path));
        acl('-a', 'everyone allow read', path);
      }

      acl('+a', 'everyone allow add_file,delete_child', root);
      await assert.rejects(checkSecretStore(store), refused(ANCESTOR_ACL, root));
      await assert.rejects(
        withSecretStore(store, async () => assert.fail()),
        refused(ANCESTOR_ACL, root),
      );
      acl('-a', 'everyone allow add_file,delete_child', root);

      // A read-only grant above the store cannot replace anything below it.
      acl('+a', 'everyone allow list,search', root);
      expect((await checkSecretStore(store)).exists).toBe(true);
    });
  },
);

test.skipIf(process.platform !== 'darwin')(
  'a slow ACL listing is waited for, and a failed one is still refused',
  async () => {
    await scratch(async (root) => {
      const saved = process.env[TEST_LS];
      const executable = join(root, 'ls');
      const log = join(root, 'ls.log');

      // The child gets only PATH and HOME, so the script carries its own paths.
      const install = (body: string) =>
        writeFile(executable, `#!/bin/sh\necho run >> '${log}'\n${body}\n`, { mode: 0o700 });

      try {
        process.env[TEST_LS] = executable;

        await install('exit 1');
        await assert.rejects(
          checkStorePaths({ directories: [join(root, 'failed')], create: [join(root, 'failed')] }),
          (error) => error instanceof SessionStoreError && error.code === 'IO',
        );

        // Longer than the 5 s bound that a loaded CI runner once hit with a working ls.
        await install('sleep 6\nexec /bin/ls "$@"');
        await checkStorePaths({ directories: [join(root, 'slow')], create: [join(root, 'slow')] });
        expect(await readFile(log, 'utf8')).toBe('run\nrun\n');
      } finally {
        if (saved === undefined) delete process.env[TEST_LS];
        else process.env[TEST_LS] = saved;
      }
    });
  },
);

test('key creation never replaces a key and leaves no temporary or partial key', async () => {
  await scratch(async (root) => {
    const { key, keys } = layout(root);
    const other = new LocalKeyFileProvider({ path: key });
    const results = await Promise.allSettled([keys.createKey(), other.createKey()]);

    expect(results.filter(({ status }) => status === 'fulfilled')).toHaveLength(1);
    const failure = results.find((result) => result.status === 'rejected');
    assert.ok(failure?.status === 'rejected');
    assert.ok(failure.reason instanceof SessionStoreError);
    expect(failure.reason.message).toBe('A store key already exists; it is never replaced.');

    const info = await stat(key);
    expect(info.size).toBe(32);
    expect(info.mode & 0o777).toBe(0o600);
    expect(info.nlink).toBe(1);
    expect(await readdir(join(root, 'family-mcp', 'keys'))).toEqual(['test-mcp.default.key']);

    const bytes = await readFile(key);
    await assert.rejects(keys.createKey(), (cause: unknown) => cause instanceof SessionStoreError);
    expect(await readFile(key)).toEqual(bytes);
  });
});

test('a crash at any key publication step restarts cleanly through the preflight', async () => {
  for (const step of ['written', 'linked'] as const)
    await scratch(async (root) => {
      const store = layout(root);
      const child = spawn(process.execPath, [worker, store.key, step], { stdio: 'inherit' });
      await once(child, 'exit');
      expect(child.signalCode).toBe('SIGKILL');
      const keyDirectory = join(root, 'family-mcp', 'keys');
      const left = await readdir(keyDirectory);

      if (step === 'written') {
        // No key yet: the temporary never became one, and setup simply runs again.
        expect(left).toHaveLength(1);
        expect((await checkSecretStore(store)).exists).toBe(false);
        await assert.rejects(
          store.keys.getKey(),
          (cause: unknown) =>
            cause instanceof SessionStoreError && cause.code === 'STORE_UNAVAILABLE',
        );
        await store.keys.createKey();
      } else {
        // The whole key, with the temporary as its second name.
        expect(left).toHaveLength(2);
        expect((await stat(store.key)).nlink).toBe(2);

        // Concurrent restarts all pass the preflight while the second name is still there.
        await Promise.all(Array.from({ length: 4 }, () => checkSecretStore(store)));
        expect(await store.keys.getKey()).toHaveLength(32);
        expect(await readdir(keyDirectory)).toEqual(['test-mcp.default.key']);
      }

      expect((await stat(store.key)).nlink).toBe(1);
      expect(await checkSecretStore(store)).toEqual({ exists: false, retired: [] });
      await withSecretRecord(store, async () => 'secret');
    });
});

test('only the store’s own temporary of the same key is recovered', async () => {
  await scratch(async (root) => {
    const { key, keys } = layout(root);
    await keys.createKey();
    const store = layout(root);

    // A hard link under any other name stays refused.
    const stray = join(root, 'family-mcp', 'keys', 'copy.key');
    await link(key, stray);
    await assert.rejects(checkSecretStore(store), refused(HARD_LINKS, key));
    await assert.rejects(
      keys.getKey(),
      (cause: unknown) => cause instanceof SessionStoreError && cause.message === HARD_LINKS,
    );
    await rm(stray);

    // A temporary name of another file is not the key's second name.
    const temporary = `${key}.${TEMPORARY}.tmp`;
    await writeFile(temporary, 'other', { mode: 0o600 });
    const outside = join(root, 'outside.key');
    await link(key, outside);
    await assert.rejects(checkSecretStore(store), refused(HARD_LINKS, key));
    expect(await readFile(temporary, 'utf8')).toBe('other');
    await rm(outside);
    await rm(temporary);

    // The recognised second name goes, and the key keeps its bytes.
    const bytes = await readFile(key);
    await link(key, temporary);
    await checkSecretStore(store);
    expect((await lstat(temporary)).nlink).toBe(2);
    expect(Buffer.from(await keys.getKey())).toEqual(bytes);
    expect(await readdir(join(root, 'family-mcp', 'keys'))).toEqual(['test-mcp.default.key']);
  });
});

test('a key whose recognised second name another process just removed passes', async () => {
  await scratch(async (root) => {
    const { key, keys } = layout(root);
    await keys.createKey();
    const temporary = `${key}.${TEMPORARY}.tmp`;
    await link(key, temporary);

    // The other process's recovery lands between this check's lstat and its own lookup.
    const recovered = async (file: string, info: Stats) => {
      await rm(temporary, { force: true });

      return (await keyTemporary(file, info)) !== undefined;
    };

    await checkStorePaths({ directories: [], files: [key], allowLink: recovered });
    expect((await lstat(key)).nlink).toBe(1);

    // A second name that stays is still refused, also when the hook does not recognise it.
    const stray = join(root, 'stray.key');
    await link(key, stray);
    await assert.rejects(
      checkStorePaths({ directories: [], files: [key], allowLink: recovered }),
      refused(HARD_LINKS, key),
    );
  });
});
