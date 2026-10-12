// Freezes complete store directory states written by the TypeScript session-store package, for
// rust/family-store/tests/{golden,persistence}.rs. Run once from the repository root with Bun
// 1.4.2 at the commit named in the provenance:
//
//   bun tooling/ts-freeze/family-store.ts rust/family-store/tests/fixtures/store
//
// Every state is synthetic: a fresh random key, made-up values, and scratch directories under
// TMPDIR. For each state it writes the files the package left (`tree/`) and what the package then
// does with a copy of them (`states.json`): its read outcome, the marker afterwards, and the outcome
// of one more write. PR 3 deletes this script with the TypeScript sources.
import {
  chmodSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';

import {
  LocalKeyFileProvider,
  SessionStoreError,
  createSecretKey,
  defaultKeyProvider,
  defaultSecretRecordPath,
  readSecretRecord,
  withSecretRecord,
  withSecretStore,
} from '../../packages/session-store/src/index.ts';

const [output] = process.argv.slice(2);

if (output === undefined) throw new RangeError('Usage: family-store.ts OUTPUT_DIRECTORY');

if (existsSync(output) && readdirSync(output).length)
  throw new RangeError('The output directory must be empty or absent.');

const scratch = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), 'ts-freeze-family-store-'));

// Explicit-path states keep the store test seam on; the default layouts set their own.
process.env.FAMILY_MCP_STORE_TEST_SEAM = '1';

type Options = Parameters<typeof readSecretRecord>[0];

/** The options of tests/common/mod.rs `options_with`: one record under `records/`. */
function explicit(root: string): Options {
  return {
    path: join(root, 'records', 'session.enc'),
    server: 'test-mcp',
    profile: 'default',
    purpose: 'session',
    schema: 1,
    keys: new LocalKeyFileProvider({ path: join(root, 'keys', 'test-mcp.default.key') }),
    maxBytes: 1024,
  };
}

async function shown(work: Promise<string | null>): Promise<string> {
  try {
    const value = await work;

    return value === null ? 'none' : `value:${value}`;
  } catch (error) {
    if (!(error instanceof SessionStoreError)) throw error;

    return `error:${error.code}`;
  }
}

const put = (options: Options, value: string) =>
  shown(withSecretRecord(options, async () => value));

/** A write whose record cannot be replaced: it stops after its pending marker (store.ts). */
async function blockedWrite(options: Options, value: string): Promise<string> {
  return shown(
    withSecretStore(options, async (store) => {
      const current = await store.read();
      const aside = `${options.path}.aside`;

      if (current !== null) renameSync(options.path, aside);
      mkdirSync(join(options.path, 'blocked'), { recursive: true });

      try {
        await store.write(value);
      } finally {
        rmSync(options.path, { recursive: true });

        if (current !== null) renameSync(aside, options.path);
      }

      return current;
    }),
  );
}

const read = (path: string) => readFileSync(path, 'utf8');

/** The committed marker text with a pending write announced, as interop.rs builds it. */
const pending = (marker: string, generation: number, nonce: string) =>
  marker.replace('}\n', `,"pending":{"generation":${generation},"nonce":"${nonce}"}}\n`);

function nonceOf(record: string): string {
  const nonce = /"nonce":"([^"]+)"/.exec(record)?.[1];

  if (nonce === undefined) throw new Error('A record without a nonce.');

  return nonce;
}

function check(outcome: string, expected: string): void {
  if (outcome !== expected) throw new Error(`expected ${expected}, got ${outcome}`);
}

type Layout = 'explicit' | 'seam' | 'macos' | 'linux';

type State = {
  id: string;
  description: string;
  layout: Layout;
  /** Values written in order, the last committed one first in line for a reader. */
  written: string[];
  build: (options: Options, root: string) => Promise<void>;
};

const created = async (options: Options) => {
  await createSecretKey(options);
};

const states: State[] = [
  {
    id: 'created',
    description: 'createSecretKey only: a key and no record',
    layout: 'explicit',
    written: [],
    build: created,
  },
  {
    id: 'one-write',
    description: 'one committed write',
    layout: 'explicit',
    written: ['from-ts-1'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'from-ts-1'), 'value:from-ts-1');
    },
  },
  {
    id: 'three-writes',
    description: 'three committed writes, the second and third not ASCII',
    layout: 'explicit',
    written: ['from-ts-1', 'frá-ts-2 þýðing 🔑', 'from-ts-3 ñ'],
    build: async (options) => {
      await created(options);

      for (const value of ['from-ts-1', 'frá-ts-2 þýðing 🔑', 'from-ts-3 ñ'])
        check(await put(options, value), `value:${value}`);
    },
  },
  {
    id: 'largest',
    description: 'a committed write of exactly maxBytes (1024) bytes',
    layout: 'explicit',
    written: ['x'.repeat(1024)],
    build: async (options) => {
      await created(options);
      check(await put(options, 'x'.repeat(1024)), `value:${'x'.repeat(1024)}`);
    },
  },
  {
    id: 'pending-first',
    description: 'a first write that stopped after its pending marker, before its record',
    layout: 'explicit',
    written: [],
    build: async (options) => {
      await created(options);
      check(await blockedWrite(options, 'lost'), 'error:STORE_WRITE_UNCERTAIN');
    },
  },
  {
    id: 'pending-later',
    description: 'a second write that stopped after its pending marker, before its record',
    layout: 'explicit',
    written: ['one'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'one'), 'value:one');
      check(await blockedWrite(options, 'lost'), 'error:STORE_WRITE_UNCERTAIN');
    },
  },
  {
    id: 'pending-landed',
    description: 'a second write whose record landed before its marker was committed',
    layout: 'explicit',
    written: ['one', 'two'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'one'), 'value:one');
      const marker1 = read(`${options.path}.marker`);
      check(await put(options, 'two'), 'value:two');
      writeFileSync(`${options.path}.marker`, pending(marker1, 2, nonceOf(read(options.path))), {
        mode: 0o600,
      });
    },
  },
  {
    id: 'pending-foreign',
    description: 'a pending marker that announces a nonce the record does not have',
    layout: 'explicit',
    written: ['one', 'two'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'one'), 'value:one');
      const marker1 = read(`${options.path}.marker`);
      check(await put(options, 'two'), 'value:two');
      writeFileSync(`${options.path}.marker`, pending(marker1, 2, 'AAAAAAAAAAAAAAAA'), {
        mode: 0o600,
      });
    },
  },
  {
    id: 'marker-behind',
    description: 'a record at generation 2 under a committed marker at generation 1',
    layout: 'explicit',
    written: ['one', 'two'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'one'), 'value:one');
      const marker1 = read(`${options.path}.marker`);
      check(await put(options, 'two'), 'value:two');
      writeFileSync(`${options.path}.marker`, marker1, { mode: 0o600 });
    },
  },
  {
    id: 'record-behind',
    description: 'a record at generation 1 under a committed marker at generation 2',
    layout: 'explicit',
    written: ['one', 'two'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'one'), 'value:one');
      const record1 = read(options.path);
      check(await put(options, 'two'), 'value:two');
      writeFileSync(options.path, record1, { mode: 0o600 });
    },
  },
  {
    id: 'lone-record',
    description: 'a committed record whose marker is gone',
    layout: 'explicit',
    written: ['one'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'one'), 'value:one');
      rmSync(`${options.path}.marker`);
    },
  },
  {
    id: 'marker-only',
    description: 'a committed marker at generation 1 whose record is gone',
    layout: 'explicit',
    written: ['one'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'one'), 'value:one');
      rmSync(options.path);
    },
  },
  {
    id: 'layout-seam',
    description: 'the default layout of test-mcp with the store test seam (absolute XDG folders)',
    layout: 'seam',
    written: ['from-ts-layout'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'from-ts-layout'), 'value:from-ts-layout');
    },
  },
  {
    id: 'layout-macos',
    description: 'the default macOS layout of test-mcp under HOME (Application Support)',
    layout: 'macos',
    written: ['from-ts-layout'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'from-ts-layout'), 'value:from-ts-layout');
    },
  },
  {
    id: 'layout-linux',
    description: 'the default Linux layout of test-mcp under HOME (~/.config, ~/.local/share)',
    layout: 'linux',
    written: ['from-ts-layout'],
    build: async (options) => {
      await created(options);
      check(await put(options, 'from-ts-layout'), 'value:from-ts-layout');
    },
  },
];

/** Run `work` with the environment and options of `layout` under `root`. */
async function inLayout<T>(
  layout: Layout,
  root: string,
  work: (options: Options) => Promise<T>,
): Promise<T> {
  const saved = { ...process.env };

  try {
    process.env.HOME = root;
    delete process.env.XDG_CONFIG_HOME;
    delete process.env.XDG_DATA_HOME;

    if (layout === 'explicit' || layout === 'seam') {
      process.env.FAMILY_MCP_STORE_TEST_SEAM = '1';
      process.env.XDG_CONFIG_HOME = join(root, 'config');
      process.env.XDG_DATA_HOME = join(root, 'data');
    } else delete process.env.FAMILY_MCP_STORE_TEST_SEAM;

    if (layout === 'explicit') return await work(explicit(root));
    const platform = layout === 'linux' ? 'linux' : 'darwin';

    if (layout === 'macos' && process.platform !== 'darwin')
      throw new RangeError('The macOS layout is generated on macOS.');

    return await work({
      path: defaultSecretRecordPath('test-mcp', { platform }),
      server: 'test-mcp',
      profile: 'default',
      purpose: 'session',
      schema: 1,
      keys: defaultKeyProvider({ server: 'test-mcp', profile: 'default', platform }),
      maxBytes: 1024,
    });
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved);
  }
}

/** Every file under `root`, relative and sorted; directories are implied. */
function files(root: string): string[] {
  return readdirSync(root, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => relative(root, join(entry.parentPath, entry.name)))
    .toSorted();
}

/** A copy of `tree` under a fresh root, owner-only, as a store directory must be. */
function plant(tree: string): string {
  const root = mkdtempSync(join(scratch, 'copy-'));
  cpSync(tree, root, { recursive: true });

  for (const name of files(root)) {
    chmodSync(join(root, name), 0o600);

    for (let folder = dirname(join(root, name)); folder !== root; folder = dirname(folder))
      chmodSync(folder, 0o700);
  }

  return root;
}

const marker = (options: Options) =>
  existsSync(`${options.path}.marker`) ? read(`${options.path}.marker`) : null;

const recorded = [];

try {
  for (const state of states) {
    const root = mkdtempSync(join(scratch, `${state.id}-`));
    await inLayout(state.layout, root, (options) => state.build(options, root));
    const tree = join(output, state.id);
    mkdirSync(tree, { recursive: true });
    cpSync(root, tree, { recursive: true });

    // What the package does with this state: read it, then write once more.
    const reader = plant(tree);

    const outcome = await inLayout(state.layout, reader, async (options) => {
      const value = await shown(readSecretRecord(options));
      const markerAfterRead = marker(options);
      const recordAfterRead = existsSync(options.path);
      const write = await put(options, 'continued');
      const reread = await shown(readSecretRecord(options));

      return {
        read: value,
        markerAfterRead,
        recordAfterRead,
        write,
        markerAfterWrite: marker(options),
        readAfterWrite: reread,
        files: files(reader),
      };
    });

    recorded.push({
      id: state.id,
      description: state.description,
      layout: state.layout,
      written: state.written,
      files: files(tree),
      modes: Object.fromEntries(
        files(root).map((name) => [name, (statSync(join(root, name)).mode & 0o777).toString(8)]),
      ),
      typescript: outcome,
    });
  }

  writeFileSync(join(output, 'states.json'), `${JSON.stringify(recorded, null, 2)}\n`);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
