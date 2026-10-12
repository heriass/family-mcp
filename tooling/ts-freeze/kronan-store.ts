// Freezes what the TypeScript Krónan package leaves on disk in the states that
// rust/kronan-mcp/tests/ts/interop.test.ts had both sides read, for rust/kronan-mcp/tests/money.rs
// and persistence.rs: a saved, a logged-out and a migrated token store, and the order-attempt
// records its `claimAttempt` writes. Run once from the repository root with Bun 1.4.2 at the commit
// named in the provenance:
//
//   BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 FAMILY_MCP_STORE_TEST_SEAM=1 \
//     bun tooling/ts-freeze/kronan-store.ts rust/kronan-mcp/tests/fixtures/store
//
// Each state runs in a scratch home of its own and is written as every file under that home: its
// path, its permission bits, and its bytes (text for `.json` files, hex otherwise). Every value is
// synthetic. PR 3 deletes this script with the TypeScript sources.
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';

import { writePrivateFile } from '../../packages/session-store/src/index.ts';
import { attemptsPath, claimAttempt, fingerprint } from '../../packages/kronan-mcp/src/attempts.ts';
import {
  logoutToken,
  migrateToken,
  saveToken,
  tokenPath,
} from '../../packages/kronan-mcp/src/auth.ts';
import { checkoutSchema } from '../../packages/kronan-mcp/src/schemas.ts';
import {
  CHECKOUT_TOKEN,
  CHECKOUT_TOTAL,
  checkout,
} from '../../rust/kronan-mcp/tests/ts/fixtures.ts';

const [output] = process.argv.slice(2);

if (output === undefined) throw new RangeError('Usage: kronan-store.ts OUTPUT_DIRECTORY');

if (existsSync(output) && readdirSync(output).length)
  throw new RangeError('The output directory must be empty or absent.');

if (process.env.FAMILY_MCP_STORE_TEST_SEAM !== '1')
  throw new RangeError('Keep FAMILY_MCP_STORE_TEST_SEAM=1; the real store is never used.');

const TOKEN = 'synthetic-token-0123456789';

const NEXT = 'synthetic-token-next-4242';

const scratch = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), 'ts-freeze-kronan-store-'));

/** interop.test.ts `fingerprint(checkoutSchema.parse(checkout))`. */
const print = fingerprint(checkoutSchema.parse(checkout));

/** What a state's TypeScript call returned: a reset flag, a migration result, a placed order. */
type Outcome = boolean | string | { orderToken: string } | null;

type State = { name: string; case: string; run: () => Promise<Outcome> };

/** The legacy token file, as versions before the store wrote it. */
const writeLegacyToken = (token: string) =>
  writePrivateFile(tokenPath(), `${JSON.stringify({ version: 1, token })}\n`);

/** A claim whose gate and send succeed at once, as interop.test.ts makes them. */
function claim(tool: 'reserve_pickup_slot' | 'complete_checkout', gate: string, total: number) {
  return claimAttempt(attemptsPath(), {
    tool,
    expectedCheckoutToken: gate,
    signal: new AbortController().signal,
    gate: async () => ({ token: gate, total, print: gate === CHECKOUT_TOKEN ? print : 'lines' }),
    send: async () => ({ orderToken: 'order-token' }),
    orderToken: (value) => value.orderToken,
  });
}

const states: State[] = [
  { name: 'saved', case: 'interop.test.ts:40', run: () => saveToken(TOKEN) },
  {
    name: 'logged-out',
    case: 'interop.test.ts:40',
    run: async () => {
      await saveToken(TOKEN);
      await logoutToken();

      return null;
    },
  },
  {
    name: 'migrated',
    case: 'interop.test.ts:65',
    run: async () => {
      await writeLegacyToken(NEXT);

      return migrateToken();
    },
  },
  {
    name: 'attempt-pickup-accepted',
    case: 'interop.test.ts:88',
    run: () => claim('reserve_pickup_slot', 'checkout-token', 990),
  },
  {
    name: 'attempt-complete-accepted',
    case: 'interop.test.ts:116',
    run: () => claim('complete_checkout', CHECKOUT_TOKEN, CHECKOUT_TOTAL),
  },
];

type Entry = { path: string; mode: string; text?: string; hex?: string };

/** Every file under `home`, sorted, with its mode and bytes. */
function files(home: string): Entry[] {
  return readdirSync(home, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => {
      const path = join(entry.parentPath, entry.name);
      const bytes = readFileSync(path);
      const mode = (statSync(path).mode & 0o777).toString(8);
      const name = relative(home, path);

      return name.endsWith('.json')
        ? { path: name, mode, text: bytes.toString('utf8') }
        : { path: name, mode, hex: bytes.toString('hex') };
    })
    .toSorted((a, b) => (a.path < b.path ? -1 : 1));
}

/** Run `work` with `home`'s environment, as parity.ts laid out each home. */
async function inHome<T>(home: string, work: () => Promise<T>): Promise<T> {
  const saved = { ...process.env };
  Object.assign(process.env, {
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_DATA_HOME: join(home, '.local/share'),
    XDG_STATE_HOME: join(home, '.local/state'),
    XDG_CACHE_HOME: join(home, '.cache'),
  });
  delete process.env.KRONAN_TOKEN_FILE;

  try {
    return await work();
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
    Object.assign(process.env, saved);
  }
}

mkdirSync(output, { recursive: true });

try {
  for (const state of states) {
    const home = join(scratch, state.name);
    mkdirSync(home, { recursive: true, mode: 0o700 });
    const result = await inHome(home, state.run);

    const frozen = {
      id: `kronan/store/${state.name}`,
      case: state.case,
      result,
      print,
      files: files(home),
    };

    const text = JSON.stringify(frozen, null, 1);

    if (text.includes(scratch)) throw new RangeError(`${state.name}: a value names the scratch.`);
    writeFileSync(join(output, `${state.name}.json`), `${text}\n`);
    process.stdout.write(`${state.name}: ${frozen.files.length} files\n`);
  }
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
