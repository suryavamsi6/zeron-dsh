import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { accessSync, constants } from 'node:fs';
import { cp, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// This test drives the installed `dsh` CLI instead of importing it, so a
// missing SDK directory or package manager skips rather than failing at import.
const sdkRoot = process.env.ZERON_DEEPSEEK_SDK_DIR;
const here = fileURLToPath(new URL('.', import.meta.url));
const fixture = resolve(here, 'fixtures/dsh-hello-bundle');
const dshBin = sdkRoot === undefined ? undefined : resolve(sdkRoot, 'node_modules/@deepseek-ai/dsh/lib/bin.js');

function onPath(command) {
  const suffixes = process.platform === 'win32' ? ['.cmd', '.exe', ''] : [''];
  return (process.env.PATH ?? '').split(delimiter).some(directory => directory !== '' && suffixes.some(suffix => {
    try {
      accessSync(join(directory, command + suffix), constants.X_OK);
      return true;
    } catch {
      return false;
    }
  }));
}

function run(args, options) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(process.execPath, args, { cwd: options.cwd, env: options.env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const timer = setTimeout(() => child.kill('SIGKILL'), options.timeout);
    child.once('error', error => { clearTimeout(timer); rejectRun(error); });
    child.once('close', (code, signal) => { clearTimeout(timer); resolveRun({ code, signal, stdout, stderr }); });
  });
}

const report = result => `exit ${result.code}${result.signal === null ? '' : ` (${result.signal})`}\n${result.stdout}\n${result.stderr}`;

test('a bundle installed in the sdk profile composes its layer and loads', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-plugins-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-plugins-bundle-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  // pnpm links the checkout it is given, so install a copy and leave the
  // fixture in the repository untouched.
  const bundle = join(checkout, 'dsh-hello-bundle');
  await cp(fixture, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));

  // The first use initializes the sdk profile from its shipped template, then
  // pnpm links the checkout and dsh appends the bundle to the profile manifest.
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  const profile = join(home, 'profiles', 'sdk');
  const manifest = JSON.parse(await readFile(join(profile, 'package.json'), 'utf8'));
  assert.ok(manifest.dependencies?.['dsh-hello-plugin'], `the profile records the dependency: ${JSON.stringify(manifest.dependencies)}`);
  const bundles = manifest.dsh?.profile?.bundles ?? [];
  assert.ok(bundles.includes('dsh-hello-plugin'), `dsh.profile.bundles lists the bundle: ${JSON.stringify(bundles)}`);
  assert.equal(bundles[0], '@deepseek-ai/dsh-base', 'the sdk template keeps its first bundle');
  assert.equal(bundles.at(-1), 'dsh-hello-plugin', 'an installed bundle is appended after the template bundles');

  // The composed tree names each layer after the bundle that contributed it.
  const dumped = await run([dshBin, '--profile', 'sdk', '--dump-config'], { cwd: checkout, env, timeout: 120000 });
  assert.equal(dumped.code, 0, `dsh --dump-config failed: ${report(dumped)}`);
  assert.match(dumped.stdout, /^# == dsh-hello-plugin$/m, 'the composed tree names the bundle layer');
  assert.match(dumped.stdout, /^- id: hello$/m);
  assert.match(dumped.stdout, /^ {2}name: dsh-hello-plugin$/m);

  // The layer's row names the package, so the profile must resolve it by name
  // and its entry point must apply.
  const loaded = await run(['--input-type=module', '-e',
    "const plugin = await import('dsh-hello-plugin'); console.log(plugin.name); plugin.apply();"], { cwd: profile, env, timeout: 60000 });
  assert.equal(loaded.code, 0, `the installed bundle does not load: ${report(loaded)}`);
  assert.match(loaded.stdout, /^hello-plugin$/m);
  assert.match(loaded.stdout, /^\[hello-plugin\] plugin loaded!$/m);
});
