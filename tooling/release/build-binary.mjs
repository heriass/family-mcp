import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { z } from 'zod';
import { readPackage } from './package.mjs';

const cargoMetadata = z.object({
  target_directory: z.string(),
  packages: z.array(
    z.object({
      id: z.string(),
      name: z.string(),
      version: z.string(),
      license: z.string().nullable(),
      source: z.string().nullable(),
      manifest_path: z.string(),
    }),
  ),
  resolve: z.object({
    nodes: z.array(
      z.object({
        id: z.string(),
        deps: z.array(
          z.object({
            pkg: z.string(),
            dep_kinds: z.array(z.object({ kind: z.string().nullable() })),
          }),
        ),
      }),
    ),
  }),
});

/**
 * Builds the crate with the compiler rust/rust-toolchain.toml pins and the crates
 * Cargo.lock pins.
 * @param {string} rust The Cargo workspace.
 * @param {string} crate
 */
function buildRust(rust, crate) {
  /** @param {string} command @param {string[]} args */
  const read = (command, args) =>
    execFileSync(command, args, { cwd: rust, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });

  // Linux archives are static musl executables, so they run on any distribution, as Bun's did.
  // macOS builds for the runner it is on.
  const requestedTarget = process.env.FAMILY_MCP_RUST_TARGET;
  assert.ok(!requestedTarget || (crate === 'infomentor-mcp' && process.platform === 'linux' && process.arch === 'x64' && requestedTarget === 'x86_64-unknown-linux-gnu'), 'Unsupported explicit Rust release target.');
  const target = requestedTarget ?? (
    process.platform === 'linux'
      ? `${process.arch === 'arm64' ? 'aarch64' : 'x86_64'}-unknown-linux-musl`
      : /^host: (.+)$/m.exec(read('rustc', ['-vV']))?.[1]);

  assert.ok(target, 'rustc did not name its host target.');
  execFileSync(
    'cargo',
    ['build', '--release', '--locked', '--package', crate, '--target', target],
    { cwd: rust, stdio: 'inherit' },
  );

  // Only the crates resolved for this target, as the build above linked them.
  const metadata = cargoMetadata.parse(
    JSON.parse(
      read('cargo', ['metadata', '--locked', '--format-version', '1', '--filter-platform', target]),
    ),
  );

  return { executable: join(metadata.target_directory, target, 'release', crate), metadata };
}

/**
 * The license texts of every crate linked into the executable.
 * @param {z.infer<typeof cargoMetadata>} metadata
 * @param {string} crate
 */
async function rustNotices({ packages, resolve: graph }, crate) {
  const nodes = new Map(graph.nodes.map((node) => [node.id, node]));
  const root = packages.find((entry) => entry.name === crate && entry.source === null);
  assert.ok(root, `Missing crate: ${crate}`);
  const linked = new Set([root.id]);

  // A Set visits the entries added while it is iterated.
  for (const id of linked)
    for (const dep of nodes.get(id)?.deps ?? [])
      if (dep.dep_kinds.some(({ kind }) => kind !== 'dev')) linked.add(dep.pkg);

  // Workspace crates (no source) are covered by the package's own LICENSE.
  const crates = packages
    .filter((entry) => linked.has(entry.id) && entry.source !== null)
    .toSorted((a, b) => `${a.name} ${a.version}`.localeCompare(`${b.name} ${b.version}`));

  assert.ok(crates.length > 0, 'Expected crate dependencies in cargo metadata.');

  const notices = [
    'This executable is compiled from Rust. The Rust standard library is licensed MIT OR Apache-2.0\n(https://www.rust-lang.org/policies/licenses). It statically links the crates below.',
  ];

  for (const entry of crates) {
    const directory = dirname(entry.manifest_path);
    const title = `${entry.name} ${entry.version}`;

    const licenses = (await readdir(directory)).filter((file) =>
      /^(?:licen[sc]e|notice|copying|unlicense)(?:[.-]|$)/i.test(file),
    );

    assert.ok(entry.license || licenses.length > 0, `Missing bundled license: ${title}`);

    // A crate published without its license file is listed by its declared SPDX expression.
    if (licenses.length === 0)
      notices.push(
        `\n\n--- ${title} ---\n\nLicense: ${entry.license} (the crate ships no license file; the license text appears under other crates in this file)\n`,
      );

    for (const file of licenses.toSorted())
      notices.push(
        `\n\n--- ${title}/${file} ---\n\n${await readFile(join(directory, file), 'utf8')}`,
      );
  }

  return { notices, dependencyCount: crates.length };
}

const { values } = parseArgs({ options: { package: { type: 'string' } } });

const pkg = await readPackage(values.package);

const bunVersion = execFileSync('bun', ['--version'], { encoding: 'utf8' }).trim();

assert.equal(
  `bun@${bunVersion}`,
  pkg.packageManager,
  'Use the pinned Bun version to build releases.',
);

assert.ok(['darwin', 'linux'].includes(process.platform), 'Supported systems: macOS and Linux.');

assert.ok(['arm64', 'x64'].includes(process.arch), 'Supported CPUs: arm64 and x64.');

const directory = await mkdtemp(join(tmpdir(), `${pkg.name}-binary-`));

const payload = join(directory, pkg.name);

const output = join(pkg.root, 'release');

try {
  await mkdir(join(payload, 'bin'), { recursive: true });
  const binary = join(payload, 'bin', pkg.name);
  const crate = pkg.familyMcp.release.rust;
  const rust = crate ? buildRust(resolve(pkg.root, '../../rust'), crate) : undefined;
  const metadata = join(directory, 'bundle.json');

  if (rust) {
    await copyFile(rust.executable, binary);
    await chmod(binary, 0o755);
  } else
    execFileSync(
      'bun',
      [
        'build',
        'src/cli.ts',
        '--compile',
        '--target',
        `bun-${process.platform}-${process.arch}`,
        '--minify',
        '--sourcemap',
        '--no-compile-autoload-dotenv',
        '--no-compile-autoload-bunfig',
        `--metafile=${metadata}`,
        '--outfile',
        binary,
      ],
      { cwd: pkg.root, stdio: 'inherit' },
    );
  assert.equal(
    execFileSync(binary, ['--version'], {
      encoding: 'utf8',
      env: { PATH: '/usr/bin:/bin' },
    }).trim(),
    pkg.version,
  );

  for (const file of ['LICENSE', 'README.md'])
    await copyFile(join(pkg.root, file), join(payload, file));

  for (const file of pkg.familyMcp.release.extraFiles) {
    if (file.platform !== process.platform) continue;
    const destination = join(payload, file.destination);
    await mkdir(dirname(destination), { recursive: true });
    await copyFile(join(pkg.root, file.source), destination);

    if (file.executable) await chmod(destination, 0o755);
  }

  /** @type {string[]} */
  let notices;
  let dependencyCount;

  if (rust && crate) ({ notices, dependencyCount } = await rustNotices(rust.metadata, crate));
  else {
    const build = z
      .object({ inputs: z.record(z.string(), z.unknown()) })
      .parse(JSON.parse(await readFile(metadata, 'utf8')));

    /** @type {Set<string>} */
    const dependencies = new Set();

    for (const input of Object.keys(build.inputs)) {
      const match = /^(.*node_modules\/(?:@[^/]+\/)?[^/]+)(?:\/|$)/.exec(input);

      if (match?.[1]) dependencies.add(resolve(pkg.root, match[1]));
    }

    assert.ok(dependencies.size > 0, 'Expected dependency inputs in Bun metafile.');
    notices = [await readFile(new URL('./Bun.txt', import.meta.url), 'utf8')];

    for (const packageRoot of [...dependencies].toSorted((a, b) => a.localeCompare(b))) {
      const { name } = z
        .object({ name: z.string() })
        .parse(JSON.parse(await readFile(join(packageRoot, 'package.json'), 'utf8')));

      const licenses = (await readdir(packageRoot)).filter((file) =>
        /^(?:licen[sc]e|notice|copying)(?:\.|$)/i.test(file),
      );

      assert.ok(licenses.length > 0, `Missing bundled license: ${name}`);

      for (const file of licenses)
        notices.push(
          `\n\n--- ${name}/${file} ---\n\n${await readFile(join(packageRoot, file), 'utf8')}`,
        );
    }

    dependencyCount = dependencies.size;
  }

  await writeFile(join(payload, 'THIRD_PARTY_NOTICES.txt'), notices.join('\n'));
  await mkdir(output, { recursive: true });
  const filename = `${pkg.name}-${pkg.version}-${process.platform}-${process.arch}.tar.gz`;
  const archive = join(output, filename);
  execFileSync('tar', ['-czf', archive, '-C', directory, pkg.name], {
    env: { ...process.env, COPYFILE_DISABLE: '1' },
  });

  const digest = createHash('sha256')
    .update(await readFile(archive))
    .digest('hex');

  await writeFile(`${archive}.sha256`, `${digest}  ${filename}\n`);
  await mkdir(join(output, 'native'), { recursive: true });
  await copyFile(binary, join(output, 'native', pkg.name));
  console.log(
    `Built ${filename} with ${crate ? 'Rust' : `Bun ${bunVersion}`}; generated notices for ${dependencyCount} bundled dependencies.`,
  );
} finally {
  await rm(directory, { recursive: true, force: true });
}
